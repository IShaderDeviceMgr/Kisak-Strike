//! Shaders: the parameter tables, the shadow phase, and the WGSL.
//!
//! Replaces `materialsystem/stdshaders/` — 166 `.cpp` files and 256 `.fxc`,
//! 54,303 lines — of which `portdocs/MATERIALSYSTEM.md` §7.8 expects 15-25
//! shaders to survive. This is the first one.
//!
//! Also replaces what is left of `materialsystem/shadersystem.cpp` once the
//! `.so` loading (§5.2), the `IShader` registration table and the snapshot
//! machinery are gone: `InitShaderParameters`' type-based defaults, and the
//! `IShader::GetParamInfo` tables that a `.vmt` is matched against.
//!
//! # The two phases, and where they went
//!
//! `shadersystem.h`'s rule is that anything affecting vertex format or fixed
//! pipeline state is decided in the **shadow** phase and hashed into an
//! immutable `StateSnapshot_t`, while anything driven by a material var at draw
//! time happens in the **dynamic** phase. `wgpu` has both natively, so:
//!
//! | Valve | Here |
//! |---|---|
//! | `IShaderShadow` calls in `SHADOW_STATE { }` | [`render_state`], which returns a [`RenderState`] — a [`PipelineKey`](super::pipeline::PipelineKey) field |
//! | `StateSnapshot_t`, `TransitionTable.cpp` | `wgpu::RenderPipeline` and the driver |
//! | `IShaderDynamicAPI` calls in `DYNAMIC_STATE { }` | the uniform blocks, written when the material is built or the frame starts |
//! | `DECLARE_STATIC_PIXEL_SHADER` + 15.3M combos | one WGSL module — see below |
//!
//! # `UnlitGeneric`'s combo bucketing
//!
//! §7.3 says to sort every `STATIC`/`DYNAMIC` axis into one of three buckets
//! and write the result down. For `UnlitGeneric` (which reaches
//! `vertexlit_and_unlit_generic_ps2x.fxc` through
//! `vertexlitgeneric_dx9_helper.cpp` with `bVertexLitGeneric = false`):
//!
//! **Bucket 1 — pinned, axis deleted.** `SFM`, `LIGHTING_PREVIEW`,
//! `TREESWAY`, `TESSELLATION`, `SEAMLESS_BASE`/`SEAMLESS_DETAIL`,
//! `SEPARATE_DETAIL_UVS`, `FLATTEN_STATIC_CONTROL_FLOW`, `SHADER_SRGB_READ`,
//! `CASCADED_SHADOW_MAPPING`, `CSM_MODE`, `CSM_BLENDING`, `COMPRESSED_VERTS`,
//! `SKINNING`, `MORPHING`, `HALFLAMBERT`, `DYNAMIC_LIGHT`, `NUM_LIGHTS`,
//! `STATICLIGHT3`, `DECAL`, and every `[CONSOLE]`/`[XBOX]`/`[SONYPS3]` gate.
//! Tools, consoles, lighting and skinning: none of them exist yet, and the ones
//! that will (skinning, lights) arrive as *data*, not as shader variants.
//!
//! **Bucket 2 — a uniform branch.** `VERTEXCOLOR` and alpha testing, both in
//! [`UnlitFlags`]. `VERTEXCOLOR` was a static combo because it changed the
//! vertex *format*; here every vertex carries a colour, so it becomes a flag
//! and a multiply. Alpha testing was not a combo at all — it was
//! fixed-function state (`EnableAlphaTest`/`AlphaFunc`), which WebGPU does not
//! have, so it becomes a `discard`.
//!
//! **Bucket 3 — a real pipeline variant.** Everything in [`RenderState`]:
//! blending, culling, depth, alpha-to-coverage, the colour write mask. Six
//! fields, and the cache key is what makes them free.
//!
//! **Deferred, not bucketed:** `$detail`, `$envmap`/`$envmapmask`, the
//! distance-alpha family (`$distancealpha`, `$outline`, `$glow`, soft edges),
//! `$decaltexture`, phong, and the flashlight. Each is a texture and a branch
//! away, and each needs content to verify against; §7.8 puts them with the
//! shaders that share them.

use bytemuck::{Pod, Zeroable};

use super::image_format::ColorSpace;
use super::pipeline::{BlendMode, DepthBias, DepthFunc, RenderState};
use super::texture::Texture;
use super::var::{MaterialFlags, MaterialVar};
use super::vmt::Vmt;

/// A shader a `.vmt` can name.
///
/// Replaces `CShaderSystem::FindShader`'s dictionary
/// (`shadersystem.cpp:1290`), which looked a name up in a `CUtlDict` populated
/// by whichever `shaderapi.so` had been `dlopen`ed. There is no registration
/// step here and no way to fail to be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderKind {
    /// Sprites, tool textures, UI in the world, and anything else whose colour
    /// is entirely in its texture. The first shader ported, because it is the
    /// smallest thing that exercises the whole path.
    UnlitGeneric,
}

impl ShaderKind {
    /// Resolves the name a `.vmt`'s outermost key gives.
    ///
    /// Case-insensitive, as `CUtlDict` with `k_eDictCompareTypeCaseInsensitive`
    /// was. Unknown names are `None`; the original substituted `Wireframe_DX9`
    /// and warned, which [`MaterialCache`](super::material::MaterialCache)
    /// improves on by substituting the error material instead — magenta
    /// checkerboard reads as "broken" to everyone, a wireframe does not.
    ///
    /// **The `_dx9`/`_dx8` suffixes are not handled and should not be.** They
    /// were fallback shaders selected by `IShader::GetFallbackShader` against a
    /// `dxlevel`, and `portdocs/MATERIALSYSTEM.md` §4.1 deletes that mechanism
    /// with the hardware variety that motivated it.
    pub fn from_name(name: &str) -> Option<ShaderKind> {
        match name {
            n if n.eq_ignore_ascii_case("UnlitGeneric") => Some(ShaderKind::UnlitGeneric),
            _ => None,
        }
    }

    /// The name content writes, in its canonical spelling.
    pub fn name(self) -> &'static str {
        match self {
            ShaderKind::UnlitGeneric => "UnlitGeneric",
        }
    }

    /// Every parameter this shader declares, standard ones first.
    ///
    /// `IShader::GetParamCount`/`GetParamInfo`, which `BEGIN_SHADER_PARAMS`
    /// builds by concatenating `CBaseShader`'s table with the shader's own
    /// (`public/shaderlib/cshader.h:212`).
    pub fn params(self) -> impl Iterator<Item = &'static ShaderParam> {
        let own = match self {
            ShaderKind::UnlitGeneric => UNLIT_GENERIC_PARAMS,
        };
        STANDARD_PARAMS.iter().chain(own)
    }

    /// The declared parameter of that name, if the shader has one.
    pub fn param(self, name: &str) -> Option<&'static ShaderParam> {
        self.params().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// The complete WGSL for this shader: the shared prelude, then the body.
    ///
    /// **This is the whole of the "how are variants expressed" mechanism**, and
    /// `portdocs/MATERIALSYSTEM.md` §10 asks that the question stay open until
    /// the prelude and a few shaders exist. It stays open: concatenation is
    /// what a `#include` of `common_ps_fxc.h` was, there are no textual
    /// variants to express yet (bucket 3 is pipeline state, bucket 2 is a
    /// uniform), and adding `naga_oil` or a build-time preprocessor before
    /// something needs one would be choosing in the dark.
    pub fn wgsl(self) -> String {
        let body = match self {
            ShaderKind::UnlitGeneric => include_str!("shaders/unlitgeneric.wgsl"),
        };
        format!("{}\n{}", include_str!("shaders/prelude.wgsl"), body)
    }
}

/// One declared parameter. `ShaderParamInfo_t`
/// (`public/materialsystem/IShader.h:66`).
///
/// The `SHADER_PARAM( NAME, TYPE, default, help )` blocks are the one part of
/// `stdshaders/` worth transliterating closely (§7.2): the names are `.vmt`
/// surface area fixed by shipped content.
///
/// **The declared default is documentation, not behaviour** — a finding worth
/// recording, because §7.2 reads as though it were live. `m_pDefaultValue` is
/// read by exactly one file in the tree, `tools/vmt/vmtdoc.cpp`, the material
/// editor. At runtime an undefined param gets a *type*-based default from
/// `CShaderSystem::InitShaderParameters` (`shadersystem.cpp:865`) or an
/// explicit one from the shader's own `SHADER_INIT_PARAMS` block. So
/// `$alphatestreference`'s `"0.7"` below never reaches a material: the default
/// that actually applies is the fixed-function alpha reference, also 0.7, set
/// somewhere else entirely (`shadershadowdx8.cpp:233`).
// `declared_default` and `help` have no reader: they are the declaration's
// own documentation, transliterated because the table is the thing being
// ported and a table that drops half of each row stops being checkable against
// the file it came from.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ShaderParam {
    /// As content spells it, `$` included, lowercase.
    pub name: &'static str,
    pub kind: ParamKind,
    /// The string in the `SHADER_PARAM` declaration. Kept because it is the
    /// documented intent even where it is not the runtime default.
    pub declared_default: &'static str,
    pub help: &'static str,
}

/// `ShaderParamType_t` (`public/materialsystem/ishader_declarations.h:38`).
///
// Declared whole even though the current shader set uses five of them: the set
// is fixed by the C++ tables being transliterated, and a type that appears in
// `stdshaders/` but not here would send the next porter back to the header to
// re-derive it.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Texture,
    Integer,
    Color,
    Vec2,
    Vec3,
    Vec4,
    Envmap,
    Float,
    Bool,
    Matrix,
    String,
}

impl ParamKind {
    /// The value an undefined parameter takes.
    ///
    /// `CShaderSystem::InitShaderParameters` (`shadersystem.cpp:865`) switches
    /// on the declared type and nothing else. `Texture` and `String` get
    /// nothing — the original leaves them undefined and the shader checks
    /// `IsTexture()` before using them, which is why a material with no
    /// `$basetexture` is a well-defined thing rather than a crash.
    pub fn default_value(self) -> Option<MaterialVar> {
        match self {
            ParamKind::Texture | ParamKind::Envmap | ParamKind::String => None,
            ParamKind::Bool | ParamKind::Integer => Some(MaterialVar::Int(0)),
            ParamKind::Color => Some(MaterialVar::Vec([1.0, 1.0, 1.0, 0.0], 3)),
            ParamKind::Vec2 => Some(MaterialVar::Vec([0.0; 4], 2)),
            ParamKind::Vec3 => Some(MaterialVar::Vec([0.0; 4], 3)),
            ParamKind::Vec4 => Some(MaterialVar::Vec([0.0; 4], 4)),
            ParamKind::Float => Some(MaterialVar::Float(0.0)),
            ParamKind::Matrix => Some(MaterialVar::Matrix(super::var::IDENTITY)),
        }
    }
}

/// `s_StandardParams` (`materialsystem/shaderlib/BaseShader.cpp:84`), minus
/// the four flag pseudo-params.
///
/// `$flags`, `$flags_defined`, `$flags2` and `$flags_defined2` were material
/// *vars* holding bit fields, because everything had to be a var to be
/// addressable by index. Here they are [`MaterialFlags`] on the
/// [`Vmt`] and never appear as parameters.
const STANDARD_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$color",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "colour modulation",
    },
    ShaderParam {
        name: "$alpha",
        kind: ParamKind::Float,
        declared_default: "1.0",
        help: "alpha modulation",
    },
    ShaderParam {
        name: "$basetexture",
        kind: ParamKind::Texture,
        declared_default: "shadertest/BaseTexture",
        help: "base texture with lighting built in",
    },
    ShaderParam {
        name: "$frame",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "animation frame",
    },
    ShaderParam {
        name: "$basetexturetransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "base texture texcoord transform",
    },
    ShaderParam {
        name: "$flashlighttexture",
        kind: ParamKind::Texture,
        declared_default: "effects/flashlight001",
        help: "flashlight spotlight shape texture",
    },
    ShaderParam {
        name: "$flashlighttextureframe",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "animation frame for $flashlighttexture",
    },
    ShaderParam {
        name: "$color2",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "second colour modulation, multiplied with $color",
    },
    ShaderParam {
        name: "$srgbtint",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "tint applied when running on new-style srgb parts",
    },
];

/// `UnlitGeneric`'s own parameters (`stdshaders/unlitgeneric_dx9.cpp:19`),
/// restricted to the ones this port reads.
///
/// The declaration there has 45 more, all of which belong to a feature listed
/// as deferred in this module's header: detail texturing, environment maps,
/// phong, distance-coded alpha, outlines and glows, decals, displacement. They
/// are left out rather than declared-and-ignored, because a table that lists a
/// parameter is a promise that setting it does something.
const UNLIT_GENERIC_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$alphatestreference",
        kind: ParamKind::Float,
        declared_default: "0.7",
        help: "alpha below which $alphatest discards a pixel",
    },
    ShaderParam {
        name: "$gammacolorread",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "disables sRGB conversion of the colour texture read",
    },
];

/// The value of a parameter, or the default an undefined one takes.
///
/// `CShaderSystem::InitShaderParameters` (`shadersystem.cpp:838`) in one
/// function. Valve wrote the defaults *into* the material's var array at
/// precache time, so that every later reader could assume every declared param
/// was defined; reading through here instead keeps a material's var list to
/// what its `.vmt` actually said, which is what makes a material printable and
/// diffable against the file it came from.
///
/// Two params are special-cased before the type-driven table, exactly as they
/// are there: `$color` becomes white and `$alpha` becomes 1, rather than the
/// black and zero their types would give.
pub fn param_value(kind: ShaderKind, vmt: &Vmt, name: &str) -> Option<MaterialVar> {
    if let Some(var) = vmt.var(name) {
        return Some(var.clone());
    }
    if name.eq_ignore_ascii_case("$color") {
        return Some(MaterialVar::Vec([1.0, 1.0, 1.0, 0.0], 3));
    }
    if name.eq_ignore_ascii_case("$alpha") {
        return Some(MaterialVar::Float(1.0));
    }
    kind.param(name)?.kind.default_value()
}

/// Where the base texture is bound within group 1.
pub const BINDING_MATERIAL_UNIFORMS: u32 = 0;
pub const BINDING_BASE_TEXTURE: u32 = 1;
pub const BINDING_BASE_SAMPLER: u32 = 2;

/// A texture a material of this kind needs, and how to read it.
#[derive(Debug, Clone, Copy)]
pub struct TextureRequest {
    /// The parameter naming it, e.g. `"$basetexture"`.
    pub param: &'static str,
    /// Where the texture goes in the material's bind group. Its sampler goes
    /// in the next binding.
    pub binding: u32,
    pub color_space: ColorSpace,
}

/// Which textures a material wants, and whether each is colour or data.
///
/// **This is where `rustdocs/MATERIALS.md`'s open rule gets encoded.** Valve
/// decided sRGB per *sampler*, in the shadow phase, with
/// `IShaderShadow::EnableSRGBRead( SHADER_SAMPLER0, ... )`; `wgpu` bakes it
/// into the texture format, so the decision has to move to load time and
/// something has to make it. That something is the shader, which is the only
/// thing that knows what it is going to do with the pixels — exactly as it was
/// before.
///
/// For `UnlitGeneric` the base texture is sRGB unless `$gammacolorread` is set,
/// which is the same test `vertexlitgeneric_dx9_helper.cpp:784` makes.
/// `$gammacolorread` is not obscure: `CMaterialSystem::CreateDebugMaterials`
/// sets it on the error material itself (`cmaterialsystem.cpp:469`).
pub fn texture_requests(kind: ShaderKind, vmt: &Vmt) -> Vec<TextureRequest> {
    match kind {
        ShaderKind::UnlitGeneric => {
            let gamma_read =
                param_value(kind, vmt, "$gammacolorread").is_some_and(|var| var.as_bool());
            vec![TextureRequest {
                param: "$basetexture",
                binding: BINDING_BASE_TEXTURE,
                color_space: if gamma_read {
                    ColorSpace::Linear
                } else {
                    ColorSpace::Srgb
                },
            }]
        }
    }
}

/// Flags in [`UnlitUniforms::flags`]. Bucket 2 of the combo split: what used to
/// be a static shader variant and is now an `if` on a uniform.
#[allow(dead_code)]
pub struct UnlitFlags;

impl UnlitFlags {
    /// `VERTEXCOLOR`, a static combo of `unlitgeneric_vs20.fxc`. Set by
    /// `$vertexcolor`.
    pub const VERTEX_COLOR: u32 = 1 << 0;
    /// Alpha testing, which D3D9 did in fixed-function state. Set by
    /// `$alphatest`.
    pub const ALPHA_TEST: u32 = 1 << 1;
    /// `SHADER_FOGMODE_DISABLED` (`BaseShader.cpp:SetFogMode`). Set by
    /// `$nofog`.
    pub const NO_FOG: u32 = 1 << 2;
}

/// `UnlitGeneric`'s material block — group 1, binding 0.
///
/// The shader-specific half of the constant ABI. The shared blocks are in
/// [`uniforms`](super::uniforms); this one lives next to the shader that reads
/// it, because its layout *is* part of the shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct UnlitUniforms {
    /// `$basetexturetransform`'s first two rows, as
    /// `SetVertexShaderTextureTransform` uploads them
    /// (`stdshaders/BaseVSShader.cpp:274`): row 0 to one register, row 1 to the
    /// next, applied with a `dot` against `(u, v, 0, 1)`.
    pub base_texture_transform: [[f32; 4]; 2],
    /// `$alphatestreference`, or the fixed-function default of 0.7.
    pub alpha_test_reference: f32,
    /// [`UnlitFlags`].
    pub flags: u32,
    /// Uniform blocks are rounded up to 16 bytes; saying so beats leaving it
    /// to `#[repr(C)]` and hoping WGSL agrees.
    pub _padding: [u32; 2],
}

/// Builds the material block for a `.vmt`.
pub fn unlit_uniforms(vmt: &Vmt) -> UnlitUniforms {
    let kind = ShaderKind::UnlitGeneric;
    let transform = param_value(kind, vmt, "$basetexturetransform")
        .map(|var| var.as_matrix())
        .unwrap_or(super::var::IDENTITY);

    // The fixed-function alpha reference `SetDefaultState` leaves in place
    // (`shadershadowdx8.cpp:233`); the helper only overrides it when
    // `$alphatestreference` is above zero (`vertexlitgeneric_dx9_helper.cpp:765`).
    let reference = param_value(kind, vmt, "$alphatestreference")
        .map(|var| var.as_f32())
        .filter(|value| *value > 0.0)
        .unwrap_or(DEFAULT_ALPHA_TEST_REFERENCE);

    let mut flags = 0;
    if vmt.flags.contains(MaterialFlags::VERTEXCOLOR) {
        flags |= UnlitFlags::VERTEX_COLOR;
    }
    if vmt.flags.contains(MaterialFlags::ALPHATEST) {
        flags |= UnlitFlags::ALPHA_TEST;
    }
    if vmt.flags.contains(MaterialFlags::NOFOG) {
        flags |= UnlitFlags::NO_FOG;
    }

    UnlitUniforms {
        base_texture_transform: [transform[0], transform[1]],
        alpha_test_reference: reference,
        flags,
        _padding: [0; 2],
    }
}

/// `AlphaFunc( SHADER_ALPHAFUNC_GEQUAL, 0.7f )` in `CShaderShadowDX8::SetDefaultState`.
const DEFAULT_ALPHA_TEST_REFERENCE: f32 = 0.7;

/// The colour a draw is modulated by: `$color * $color2`, with `$alpha` in `w`.
///
/// `CBaseMeshDX8::DrawMesh` (`shaderapidx9/meshdx8.cpp:2378`) reads `$color`
/// and `$alpha` into the instance's diffuse modulation, and
/// `CBICMD_SETMODULATIONVERTEXSHADERDYNAMICSTATE`
/// (`shaderapidx8.cpp:8875`) multiplies that by `$color2` on the way to
/// `cModulationColor`.
///
/// **`$srgbtint` is deliberately not applied.** `ApplyColor2Factor`
/// (`BaseShader.cpp:731`) folds it in too — but only when
/// `UsesSRGBCorrectBlending()`, and it linearizes it for the vertex path while
/// leaving it gamma for the pixel path, which cannot both be right. It defaults
/// to white, and no Portal 2 content found so far sets it.
///
/// **The value stays in gamma space**, which is Valve's behaviour and looks
/// wrong until you follow it through: the texture sample is linear (the
/// hardware decoded it), and this multiplies it un-linearized. Content was
/// authored against that, so `$color "[.5 .5 .5]"` has to keep meaning what it
/// meant in 2011 rather than what it should have meant.
pub fn modulation_color(kind: ShaderKind, vmt: &Vmt) -> [f32; 4] {
    let value = |name| param_value(kind, vmt, name);
    let color = value("$color").map(|var| var.as_vec4()).unwrap_or([1.0; 4]);
    let color2 = value("$color2")
        .map(|var| var.as_vec4())
        .unwrap_or([1.0; 4]);
    let alpha = value("$alpha").map(|var| var.as_f32()).unwrap_or(1.0);

    [
        color[0] * color2[0],
        color[1] * color2[1],
        color[2] * color2[2],
        alpha,
    ]
}

/// The shadow phase: the pipeline state a material asks for.
///
/// Two layers of the original, in order:
///
/// 1. `CBaseShader::SetInitialShadowState` (`shaderlib/BaseShader.cpp:183`),
///    which every shader gets for free — the flags that map straight onto fixed
///    pipeline state.
/// 2. `CBaseVSShader::EvaluateBlendRequirements` (`BaseVSShader.cpp:700`) and
///    `SetBlendingShadowState`, which decide blending from the flags *and from
///    whether the base texture has an alpha channel*.
///
/// `base_texture` is the resolved `$basetexture`, because step 2 cannot be
/// answered without it.
///
/// # What alpha modulation costs, and why it is not final
///
/// `bIsAlphaModulating` is a *draw-time* input in the original: it comes from
/// the instance's diffuse modulation alpha (`shaderapidx8.cpp:4870`), which is
/// `$alpha` times whatever `IMatRenderContext::OverrideAlpha` set. That is why
/// a material carried up to eight state snapshots. Here it is read from
/// `$alpha` alone, because there is no render context to override it yet; when
/// there is, this becomes an argument and [`RenderState`] stays exactly as it
/// is — the pipeline cache already keys on it.
pub fn render_state(kind: ShaderKind, vmt: &Vmt, base_texture: Option<&Texture>) -> RenderState {
    let ShaderKind::UnlitGeneric = kind;
    let flags = vmt.flags;
    let mut state = RenderState::default();

    // --- SetInitialShadowState -------------------------------------------
    if flags.contains(MaterialFlags::IGNOREZ) {
        state.depth_test = false;
        state.depth_write = false;
    }
    if flags.contains(MaterialFlags::DECAL) {
        state.depth_bias = DepthBias::Decal;
        state.depth_write = false;
    }
    if flags.contains(MaterialFlags::NOCULL) {
        state.cull = false;
    }
    if flags.contains(MaterialFlags::ZNEARER) {
        state.depth_func = DepthFunc::Nearer;
    }
    if flags.contains(MaterialFlags::ALLOWALPHATOCOVERAGE) {
        state.alpha_to_coverage = true;
    }
    // `$wireframe` asked for `PolyMode( FRONT_AND_BACK, LINE )`. `wgpu`'s
    // `PolygonMode::Line` needs `Features::POLYGON_MODE_LINE`, which is outside
    // the single capability tier of `portdocs/MATERIALSYSTEM.md` §4.6 and is
    // not available on all of it — Metal has no line fill mode at all. A
    // wireframe material draws solid. The debug shaders that wanted this
    // (`debugwireframe`, `wireframe.cpp`) are not in §7.8's target set.

    // --- EvaluateBlendRequirements ---------------------------------------
    let alpha_test = flags.contains(MaterialFlags::ALPHATEST);
    let alpha_modulating = param_value(kind, vmt, "$alpha").is_some_and(|var| var.as_f32() != 1.0);
    let translucent = alpha_modulating
        || flags.contains(MaterialFlags::VERTEXALPHA)
        || (base_texture_is_translucent(vmt, base_texture) && !alpha_test);

    state.blend = if flags.contains(MaterialFlags::ADDITIVE) {
        if translucent {
            BlendMode::BlendAdd
        } else {
            BlendMode::Add
        }
    } else if translucent {
        BlendMode::Blend
    } else {
        BlendMode::None
    };
    // `EnableAlphaBlending` turns depth writes off as well as blending on
    // (`BaseShader.cpp:781`) — one call, two effects, and the second one is
    // easy to miss.
    if state.blend != BlendMode::None {
        state.depth_write = false;
    }

    // "HACK HACK HACK - enable alpha writes all the time so that we have them
    // for underwater stuff" (`vertexlitgeneric_dx9_helper.cpp:1206`). Alpha
    // writes are *off* in the default state, so an opaque material is the only
    // kind that writes the frame's alpha channel — which is what the underwater
    // fog pass then reads.
    //
    // **Computed before the `$multiply` override below, and that ordering is
    // the original's**: `bFullyOpaque` reads `nBlendType`
    // (`vertexlitgeneric_dx9_helper.cpp:583`), the value the blend evaluation
    // produced, not the mode `$multiply` replaces it with. So a `$multiply`
    // material that is *also* alpha-modulated does not write alpha, even though
    // `Multiply` is not one of the two blend modes named here.
    state.write_alpha =
        !matches!(state.blend, BlendMode::Blend | BlendMode::BlendAdd) && !alpha_test;

    // `IS_FLAG_SET( MATERIAL_VAR_MULTIPLY )` at the end of the shadow block
    // (`vertexlitgeneric_dx9_helper.cpp:1210`), after everything above.
    if flags.contains(MaterialFlags::MULTIPLY) {
        state.blend = BlendMode::Multiply;
        state.depth_write = false;
    }

    state
}

/// `CBaseShader::TextureIsTranslucent( BASETEXTURE, true )`
/// (`shaderlib/BaseShader.cpp:605`).
///
/// Not simply "does the texture have alpha": the base texture's alpha channel
/// is *shared*, and three flags claim it for something other than translucency.
/// If any of them does, the material is opaque no matter what the `.vtf`
/// contains.
fn base_texture_is_translucent(vmt: &Vmt, base_texture: Option<&Texture>) -> bool {
    // The original's first test is `GetType() == MATERIAL_VAR_TYPE_TEXTURE` —
    // "did the `.vmt` actually name one" — which has no counterpart here
    // because a material always ends up with *something* bound. It needs none:
    // both substitutes, the white texture and the error checkerboard, are
    // created without an alpha flag and so report themselves opaque, which is
    // the same answer.
    let Some(texture) = base_texture else {
        return false;
    };
    let flags = vmt.flags;

    if flags.contains(MaterialFlags::OPAQUETEXTURE) {
        return false;
    }
    // `MATERIAL_VAR2_SELFILLUMMASK` is a flags2 bit set by the shader, not by
    // content; with `$selfillummask` unported, `$selfillum` always means "the
    // base alpha is the mask".
    let self_illum_uses_base_alpha = flags.contains(MaterialFlags::SELFILLUM);
    if self_illum_uses_base_alpha || flags.contains(MaterialFlags::BASEALPHAENVMAPMASK) {
        return false;
    }
    if !flags.contains(MaterialFlags::TRANSLUCENT) && !flags.contains(MaterialFlags::ALPHATEST) {
        return false;
    }
    texture.is_translucent()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::keyvalues;

    fn vmt(body: &str) -> Vmt {
        let text = format!("\"UnlitGeneric\" {{ {body} }}");
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        Vmt::from_keyvalues("test.vmt", &document).expect("a shader block")
    }

    #[test]
    fn shader_names_resolve_case_insensitively() {
        assert_eq!(
            ShaderKind::from_name("unlitgeneric"),
            Some(ShaderKind::UnlitGeneric)
        );
        assert_eq!(
            ShaderKind::from_name("UnlitGeneric"),
            Some(ShaderKind::UnlitGeneric)
        );
        // A fallback name is not a shader: that mechanism is deleted.
        assert_eq!(ShaderKind::from_name("UnlitGeneric_dx9"), None);
        assert_eq!(ShaderKind::from_name("LightmappedGeneric"), None);
    }

    #[test]
    fn the_standard_parameters_are_declared() {
        let kind = ShaderKind::UnlitGeneric;
        for name in [
            "$color",
            "$alpha",
            "$basetexture",
            "$frame",
            "$basetexturetransform",
            "$alphatestreference",
        ] {
            assert!(kind.param(name).is_some(), "{name}");
        }
        assert!(kind.param("$BASETEXTURE").is_some(), "case-insensitive");
        assert!(kind.param("$bumpmap").is_none(), "not an unlit parameter");
    }

    #[test]
    fn undefined_parameters_take_their_type_default() {
        assert_eq!(
            ParamKind::Float.default_value(),
            Some(MaterialVar::Float(0.0))
        );
        assert_eq!(ParamKind::Bool.default_value(), Some(MaterialVar::Int(0)));
        assert_eq!(
            ParamKind::Color.default_value(),
            Some(MaterialVar::Vec([1.0, 1.0, 1.0, 0.0], 3)),
            "colours default to white, not to black"
        );
        assert_eq!(ParamKind::Texture.default_value(), None);
    }

    #[test]
    fn the_base_texture_is_srgb_unless_the_material_says_otherwise() {
        let requests = texture_requests(ShaderKind::UnlitGeneric, &vmt(r#""$basetexture" "x""#));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].param, "$basetexture");
        assert_eq!(requests[0].color_space, ColorSpace::Srgb);

        let requests = texture_requests(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$basetexture" "x" "$gammacolorread" "1""#),
        );
        assert_eq!(requests[0].color_space, ColorSpace::Linear);
    }

    #[test]
    fn modulation_multiplies_the_two_colours_and_carries_alpha_in_w() {
        let modulation = modulation_color(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$color" "[0.5 1 1]" "$color2" "[1 0.5 1]" "$alpha" "0.25""#),
        );
        assert_eq!(modulation, [0.5, 0.5, 1.0, 0.25]);

        // Nothing set is white and opaque.
        assert_eq!(
            modulation_color(ShaderKind::UnlitGeneric, &vmt("")),
            [1.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn the_default_state_is_opaque_depth_tested_and_culled() {
        let state = render_state(ShaderKind::UnlitGeneric, &vmt(""), None);
        assert_eq!(state.blend, BlendMode::None);
        assert!(state.depth_test && state.depth_write);
        assert_eq!(state.depth_func, DepthFunc::NearerOrEqual);
        assert!(state.cull);
        assert!(
            state.write_alpha,
            "an opaque material writes dest alpha for the underwater pass"
        );
    }

    #[test]
    fn flags_map_onto_fixed_pipeline_state() {
        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$ignorez" "1""#), None);
        assert!(!state.depth_test && !state.depth_write);

        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$nocull" "1""#), None);
        assert!(!state.cull);

        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$znearer" "1""#), None);
        assert_eq!(state.depth_func, DepthFunc::Nearer);

        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$decal" "1""#), None);
        assert_eq!(state.depth_bias, DepthBias::Decal);
        assert!(!state.depth_write, "a decal must not write depth");
    }

    #[test]
    fn alpha_modulation_alone_makes_a_material_translucent() {
        // No texture, no $translucent — just an alpha below one, which is what
        // `EvaluateBlendRequirements` checks first.
        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$alpha" "0.5""#), None);
        assert_eq!(state.blend, BlendMode::Blend);
        assert!(!state.depth_write, "blending turns depth writes off");
        assert!(!state.write_alpha);

        // And exactly one is opaque.
        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$alpha" "1""#), None);
        assert_eq!(state.blend, BlendMode::None);
    }

    #[test]
    fn additive_and_multiply_pick_their_blend_modes() {
        let state = render_state(ShaderKind::UnlitGeneric, &vmt(r#""$additive" "1""#), None);
        assert_eq!(state.blend, BlendMode::Add);

        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$additive" "1" "$alpha" "0.5""#),
            None,
        );
        assert_eq!(state.blend, BlendMode::BlendAdd);

        // `$multiply` is applied last and overrides whatever came before.
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$multiply" "1" "$additive" "1""#),
            None,
        );
        assert_eq!(state.blend, BlendMode::Multiply);
        assert!(!state.depth_write);
        assert!(
            state.write_alpha,
            "an opaque $multiply material still writes alpha"
        );

        // But alpha writes are decided from the blend mode *before* the
        // override, so an alpha-modulated one does not — even though `Multiply`
        // is neither `Blend` nor `BlendAdd`.
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$multiply" "1" "$alpha" "0.5""#),
            None,
        );
        assert_eq!(state.blend, BlendMode::Multiply);
        assert!(!state.write_alpha);
    }

    #[test]
    fn alpha_tested_materials_are_opaque_and_discard() {
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$translucent" "0" "$alphatest" "1""#),
            None,
        );
        assert_eq!(state.blend, BlendMode::None, "alpha test is not blending");
        assert!(
            !state.write_alpha,
            "an alpha-tested material is not fully opaque"
        );

        let uniforms = unlit_uniforms(&vmt(r#""$alphatest" "1""#));
        assert_eq!(
            uniforms.flags & UnlitFlags::ALPHA_TEST,
            UnlitFlags::ALPHA_TEST
        );
        assert_eq!(uniforms.alpha_test_reference, 0.7, "the default reference");

        let uniforms = unlit_uniforms(&vmt(r#""$alphatest" "1" "$alphatestreference" "0.3""#));
        assert_eq!(uniforms.alpha_test_reference, 0.3);
        // Zero means "not set", and leaves the fixed-function default alone.
        let uniforms = unlit_uniforms(&vmt(r#""$alphatestreference" "0""#));
        assert_eq!(uniforms.alpha_test_reference, 0.7);
    }

    #[test]
    fn the_texture_transform_reaches_the_uniform_as_two_rows() {
        let uniforms = unlit_uniforms(&vmt(
            r#""$basetexturetransform" "center 0 0 scale 1 1 rotate 0 translate .25 .5""#,
        ));
        // Row-major rows, applied with a dot against (u, v, 0, 1) — so the
        // translation is the fourth component of each row.
        assert!((uniforms.base_texture_transform[0][3] - 0.25).abs() < 1e-6);
        assert!((uniforms.base_texture_transform[1][3] - 0.5).abs() < 1e-6);
        assert!((uniforms.base_texture_transform[0][0] - 1.0).abs() < 1e-6);

        // Unset is the identity, matching `SetVertexShaderTextureTransform`.
        let uniforms = unlit_uniforms(&vmt(""));
        assert_eq!(uniforms.base_texture_transform[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(uniforms.base_texture_transform[1], [0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn vertex_colour_and_fog_are_uniform_flags() {
        let uniforms = unlit_uniforms(&vmt(r#""$vertexcolor" "1" "$nofog" "1""#));
        assert_eq!(
            uniforms.flags & UnlitFlags::VERTEX_COLOR,
            UnlitFlags::VERTEX_COLOR
        );
        assert_eq!(uniforms.flags & UnlitFlags::NO_FOG, UnlitFlags::NO_FOG);

        assert_eq!(unlit_uniforms(&vmt("")).flags, 0);
    }

    #[test]
    fn the_material_block_is_the_size_wgsl_expects() {
        assert_eq!(size_of::<UnlitUniforms>(), 48);
        assert_eq!(size_of::<UnlitUniforms>() % 16, 0);
    }
}
