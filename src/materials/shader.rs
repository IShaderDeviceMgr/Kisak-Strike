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
use super::mesh::VertexLayout;
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
// `clippy::enum_variant_names`: every variant ends in `Generic`, and all three
// names are content surface area — a `.vmt`'s outermost key is matched against
// them (`ShaderKind::from_name`), so renaming one to satisfy a lint would
// break every material in the game.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderKind {
    /// Sprites, tool textures, UI in the world, and anything else whose colour
    /// is entirely in its texture. The first shader ported, because it is the
    /// smallest thing that exercises the whole path.
    UnlitGeneric,

    /// World brush surfaces: a base texture multiplied by a baked lightmap.
    /// The shader almost every wall, floor and ceiling in a Source map names —
    /// 62 of `sp_a1_intro1`'s 66 world materials.
    ///
    /// `stdshaders/lightmappedgeneric_dx9.cpp` through
    /// `lightmappedgeneric_dx9_helper.cpp`, `lightmappedgeneric_vs20.fxc` and
    /// `lightmappedgeneric_ps2_3_x.h`.
    LightmappedGeneric,

    /// Models: props, characters, gibs and debris. A base texture lit by an
    /// ambient cube, up to four local lights, and whatever `vrad` baked into
    /// the vertex stream — 1,012 of Portal 2's 1,096 `materials/models/`
    /// materials, and the largest single shader in the shipped game.
    ///
    /// `stdshaders/vertexlitgeneric_dx9.cpp` through
    /// `vertexlitgeneric_dx9_helper.cpp`, `vertexlit_and_unlit_generic_vs20.fxc`
    /// and `vertexlit_and_unlit_generic_ps2x.fxc` — plus the `_bump_` pair of
    /// the same names, which is the same shader with a normal map and is one
    /// WGSL module here.
    ///
    /// **A `.vmt` naming `VertexLitGeneric` does not always reach this
    /// shader.** `DrawVertexLitGeneric_DX9` (`vertexlitgeneric_dx9_helper.cpp:2346`)
    /// opens by handing the material to `DrawPhong_DX9` when `WantsPhongShader`
    /// says so — `$phong 1` plus any of a `$bumpmap`, a `$lightwarptexture` or
    /// `$basemapalphaphongmask 1`. That is 307 of Portal 2's 1,108
    /// `VertexLitGeneric` materials, and `Phong` is a separate entry in
    /// `portdocs/MATERIALSYSTEM.md` §7.8 that is not ported: they draw here
    /// without their specular. See [`wants_phong`].
    VertexLitGeneric,
}

/// What a shader binds in group 3, if anything.
///
/// Group 3 is "where this shader's lighting comes from", and the two answers
/// so far are the two ways Source lights a surface: a page of the baked
/// lightmap atlas for brushes, an ambient cube plus local lights for models.
/// A pipeline layout is per shader, so a shader that reads neither declares no
/// group 3 at all and its draws bind nothing there.
///
/// Groups 0, 1 and 2 are frequency groups shared by every shader
/// ([`uniforms`](super::uniforms)); this one is the exception, and it is an
/// exception Valve had too — `BindLightmapPage` and
/// `PI_SetVertexShaderAmbientLightCube` are both render-context state that
/// neither the material nor the draw call owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightingBinding {
    /// A lightmap atlas page: a texture and its sampler.
    /// [`Pass::bind_lightmap_page`](super::context::Pass::bind_lightmap_page).
    LightmapPage,
    /// [`ModelLighting`](super::uniforms::ModelLighting), bound with a dynamic
    /// offset. [`Pass::set_model_lighting`](super::context::Pass::set_model_lighting).
    ModelLighting,
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
            n if n.eq_ignore_ascii_case("LightmappedGeneric") => {
                Some(ShaderKind::LightmappedGeneric)
            }
            n if n.eq_ignore_ascii_case("VertexLitGeneric") => Some(ShaderKind::VertexLitGeneric),
            _ => None,
        }
    }

    /// The name content writes, in its canonical spelling.
    pub fn name(self) -> &'static str {
        match self {
            ShaderKind::UnlitGeneric => "UnlitGeneric",
            ShaderKind::LightmappedGeneric => "LightmappedGeneric",
            ShaderKind::VertexLitGeneric => "VertexLitGeneric",
        }
    }

    /// The vertex layout this shader reads.
    ///
    /// `IShaderShadow::VertexShaderVertexFormat( flags, nTexCoords, pDims,
    /// nUserDataSize )`, called by every shader's shadow phase — the vertex
    /// format was always the *shader's* declaration rather than the mesh's, and
    /// keeping it there is what lets [`PipelineKey`](super::pipeline::PipelineKey)
    /// stay a `ShaderKind` plus state instead of growing a layout field.
    ///
    /// It grows a field the day a shader has two layouts, and
    /// `portdocs/MATERIALSYSTEM.md` §10 expected `LightmappedGeneric`'s bumped
    /// variant to be it. **It is not**, and the reason is worth knowing: the
    /// bumped diffuse path dots a *tangent-space* normal against a constant
    /// basis (`lightmappedgeneric_ps2_3_x.h:665`) and never needs a
    /// world-space frame, so the shadow phase adds `VERTEX_TANGENT_S |
    /// VERTEX_TANGENT_T | VERTEX_NORMAL` only for an `$envmap`
    /// (`lightmappedgeneric_dx9_helper.cpp:670`). Bumped and unbumped read the
    /// same layout, and bumped lighting is a flag in a uniform — §7.3's bucket
    /// 2 rather than bucket 3.
    ///
    /// `VertexLitGeneric` *is* a shader with two of Valve's layouts — the
    /// tangent is `userDataSize = 4` only when the material is bumped
    /// (`vertexlitgeneric_dx9_helper.cpp:824`) — and this port still answers
    /// with one, because the tangent is in the `.vvd` either way. The reasoning
    /// and the condition to revisit are on
    /// [`ModelVertex`](super::mesh::ModelVertex).
    pub fn vertex_layout(self) -> VertexLayout {
        match self {
            // `unlitgeneric_vs20.fxc`'s `VS_INPUT` also declares `vNormal`,
            // `vBoneWeights` and `vBoneIndices`; with lighting and skinning off
            // — which is what `unlitgeneric_dx9.cpp` asks the shared helper for
            // — nothing reads them.
            ShaderKind::UnlitGeneric => VertexLayout::Simple,
            // `VertexShaderVertexFormat( VERTEX_POSITION, numTexCoords, 0, 0 )`
            // (`lightmappedgeneric_dx9_helper.cpp:681`). The third texture
            // coordinate is the bumped one, and Portal 2 writes it in both
            // cases anyway — "PORTAL 2 FIX - paint shader assumes it can use 3
            // lightmapped coordinates in all cases"
            // (`matsys_interface.cpp:1502`).
            ShaderKind::LightmappedGeneric => VertexLayout::World,
            // `VertexShaderVertexFormat( VERTEX_POSITION | VERTEX_NORMAL |
            // VERTEX_COLOR_STREAM_1, 1, {2}, userDataSize )`
            // (`vertexlitgeneric_dx9_helper.cpp:895`).
            ShaderKind::VertexLitGeneric => VertexLayout::Model,
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
            ShaderKind::LightmappedGeneric => LIGHTMAPPED_GENERIC_PARAMS,
            ShaderKind::VertexLitGeneric => VERTEX_LIT_GENERIC_PARAMS,
        };
        STANDARD_PARAMS.iter().chain(own)
    }

    /// The declared parameter of that name, if the shader has one.
    pub fn param(self, name: &str) -> Option<&'static ShaderParam> {
        self.params().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// What draws of this shader bind in group 3.
    ///
    /// `IMatRenderContext::BindLightmapPage` applied to every shader whether it
    /// read one or not, and `PI_SetVertexShaderAmbientLightCube` was emitted
    /// only by the shaders that wanted it; here both decide a pipeline layout,
    /// so both have to be answerable per shader. See [`LightingBinding`].
    pub fn lighting_binding(self) -> Option<LightingBinding> {
        match self {
            ShaderKind::UnlitGeneric => None,
            ShaderKind::LightmappedGeneric => Some(LightingBinding::LightmapPage),
            ShaderKind::VertexLitGeneric => Some(LightingBinding::ModelLighting),
        }
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
            ShaderKind::LightmappedGeneric => include_str!("shaders/lightmappedgeneric.wgsl"),
            ShaderKind::VertexLitGeneric => include_str!("shaders/vertexlitgeneric.wgsl"),
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

/// `LightmappedGeneric`'s own parameters
/// (`stdshaders/lightmappedgeneric_dx9.cpp:12`), restricted to the ones this
/// port reads.
///
/// The declaration there has around sixty more. They belong to `$basetexture2`
/// blending, detail texturing, environment maps, self-illumination, phong,
/// seamless mapping, the paint shader and the flashlight — every one of them
/// listed as deferred in this module's header — and are left out rather than
/// declared-and-ignored, because a table that lists a parameter is a promise
/// that setting it does something.
const LIGHTMAPPED_GENERIC_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$bumpmap",
        kind: ParamKind::Texture,
        declared_default: "models/shadertest/shader1_normal",
        help: "bump map",
    },
    ShaderParam {
        name: "$bumpframe",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "frame number for $bumpmap",
    },
    ShaderParam {
        name: "$bumptransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "$bumpmap texcoord transform",
    },
    ShaderParam {
        name: "$nodiffusebumplighting",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "0 == diffuse bumpmapping, 1 == no diffuse bumpmapping",
    },
    ShaderParam {
        name: "$forcebump",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "0 == Do bumpmapping if the config allows it, 1 == do it regardless",
    },
    ShaderParam {
        name: "$alphatestreference",
        kind: ParamKind::Float,
        declared_default: "0.0",
        help: "alpha below which $alphatest discards a pixel",
    },
];

/// `VertexLitGeneric`'s own parameters (`stdshaders/vertexlitgeneric_dx9.cpp:19`),
/// restricted to the ones this port reads.
///
/// The declaration there has around 130 more, and they fall into three groups.
/// **Other passes**: the emissive-scroll, cloak and flesh-interior blended
/// passes, which are three whole extra shaders drawn over the top of this one
/// (`emissive_scroll_blended_pass_helper.cpp` and friends) and are HL2/Alien
/// Swarm content, not Portal 2's. **Other shaders**: everything `$phong` pulls
/// in, which reaches `phong_dx9_helper.cpp` instead — see
/// [`wants_phong`]. **Features not ported**: wrinkle maps, tree sway, decal
/// textures, tint masks, displacement, distance alpha, seamless mapping,
/// self-illum fresnel, the flashlight and cascaded shadow maps.
///
/// They are left out rather than declared-and-ignored, because a table that
/// lists a parameter is a promise that setting it does something.
const VERTEX_LIT_GENERIC_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$bumpmap",
        kind: ParamKind::Texture,
        declared_default: "models/shadertest/shader1_normal",
        help: "bump map",
    },
    ShaderParam {
        name: "$bumpframe",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "frame number for $bumpmap",
    },
    ShaderParam {
        name: "$bumptransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "$bumpmap texcoord transform",
    },
    ShaderParam {
        name: "$selfillumtint",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "Self-illumination tint",
    },
    ShaderParam {
        name: "$selfillummask",
        kind: ParamKind::Texture,
        declared_default: "shadertest/BaseTexture",
        help: "If we bind a texture here, it overrides base alpha (if any) for self illum",
    },
    ShaderParam {
        name: "$selfillummaskscale",
        kind: ParamKind::Float,
        declared_default: "0",
        help: "Scale self illum effect strength",
    },
    ShaderParam {
        name: "$detail",
        kind: ParamKind::Texture,
        declared_default: "shadertest/detail",
        help: "detail texture",
    },
    ShaderParam {
        name: "$detailframe",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "frame number for $detail",
    },
    ShaderParam {
        name: "$detailscale",
        kind: ParamKind::Float,
        declared_default: "4",
        help: "scale of the detail texture",
    },
    ShaderParam {
        name: "$detailtint",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "detail texture tint",
    },
    ShaderParam {
        name: "$detailblendmode",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "mode for combining detail texture with base",
    },
    ShaderParam {
        name: "$detailblendfactor",
        kind: ParamKind::Float,
        declared_default: "1",
        help: "blend amount for detail texture",
    },
    ShaderParam {
        name: "$detailtexturetransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "$detail texcoord transform",
    },
    ShaderParam {
        name: "$envmap",
        kind: ParamKind::Envmap,
        declared_default: "shadertest/shadertest_env",
        help: "envmap",
    },
    ShaderParam {
        name: "$envmapframe",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "envmap frame number",
    },
    ShaderParam {
        name: "$envmapmask",
        kind: ParamKind::Texture,
        declared_default: "shadertest/shadertest_envmask",
        help: "envmap mask",
    },
    ShaderParam {
        name: "$envmaptint",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "envmap tint",
    },
    ShaderParam {
        name: "$envmapcontrast",
        kind: ParamKind::Float,
        declared_default: "0.0",
        help: "contrast 0 == normal 1 == color*color",
    },
    ShaderParam {
        name: "$envmapsaturation",
        kind: ParamKind::Float,
        declared_default: "1.0",
        help: "saturation 0 == greyscale 1 == normal",
    },
    ShaderParam {
        name: "$envmapfresnel",
        kind: ParamKind::Float,
        declared_default: "0",
        help: "Degree to which Fresnel should be applied to env map",
    },
    ShaderParam {
        name: "$envmapfresnelminmaxexp",
        kind: ParamKind::Vec3,
        declared_default: "[0.0 1.0 2.0]",
        help: "Min/max fresnel range and exponent for vertexlitgeneric",
    },
    ShaderParam {
        name: "$basealphaenvmapmaskminmaxexp",
        kind: ParamKind::Vec3,
        declared_default: "[1.0 0.0 1.0]",
        help: "Min/max range and exponent for $basealphaenvmapmask",
    },
    ShaderParam {
        name: "$blendtintbybasealpha",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "Use the base alpha to blend in the $color modulation",
    },
    ShaderParam {
        name: "$notint",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "Disable tinting",
    },
    ShaderParam {
        name: "$allowdiffusemodulation",
        kind: ParamKind::Bool,
        declared_default: "1",
        help: "Allow per-instance color modulation",
    },
    ShaderParam {
        name: "$phong",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "enables phong lighting",
    },
    ShaderParam {
        name: "$basemapalphaphongmask",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "indicates that there is no normal map and that the phong mask is in base alpha",
    },
    ShaderParam {
        name: "$lightwarptexture",
        kind: ParamKind::Texture,
        declared_default: "shadertest/BaseTexture",
        help: "1D ramp texture for tinting scalar diffuse term",
    },
    ShaderParam {
        name: "$alphatestreference",
        kind: ParamKind::Float,
        declared_default: "0.0",
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

/// `InitFloatParam( index, params, default )` (`stdshaders/BaseVSShader.h`).
///
/// **[`param_value`] cannot express this, and the difference is a silent
/// zero.** There are *two* default mechanisms in the original and they run in
/// order:
///
/// 1. `SHADER_INIT_PARAMS` — the shader's own `InitParams*` function, which
///    *writes* a real value into the var array for a parameter the `.vmt` left
///    out. `$detailscale` becomes 4, `$envmapsaturation` becomes 1.
/// 2. `CShaderSystem::InitShaderParameters` (`shadersystem.cpp:865`), which
///    fills anything still undefined from its declared *type* — 0 for a float,
///    black for a colour.
///
/// [`param_value`] is the second. Reaching for it and then writing
/// `.unwrap_or( 4.0 )` looks right and is dead code: the type default arrives
/// first and the fallback never fires, so `$detailscale` silently becomes 0 and
/// every detail texture collapses to a single texel. This function is the first
/// mechanism, and anything with a non-type default must go through it.
fn init_float(vmt: &Vmt, name: &str, default: f32) -> f32 {
    vmt.var(name).map(|var| var.as_f32()).unwrap_or(default)
}

/// [`init_float`] for a `SetVecValue` default.
fn init_vec(vmt: &Vmt, name: &str, default: [f32; 4]) -> [f32; 4] {
    vmt.var(name).map(|var| var.as_vec4()).unwrap_or(default)
}

/// Where each texture is bound within group 1. A texture's sampler always
/// goes in the binding after it, which is what lets
/// [`Material::new`](super::material::Material::new) fill the group from
/// [`texture_requests`] alone.
pub const BINDING_MATERIAL_UNIFORMS: u32 = 0;
pub const BINDING_BASE_TEXTURE: u32 = 1;
pub const BINDING_BASE_SAMPLER: u32 = 2;
pub const BINDING_BUMP_TEXTURE: u32 = 3;
pub const BINDING_BUMP_SAMPLER: u32 = 4;
pub const BINDING_DETAIL_TEXTURE: u32 = 5;
pub const BINDING_DETAIL_SAMPLER: u32 = 6;
pub const BINDING_SELFILLUM_MASK_TEXTURE: u32 = 7;
pub const BINDING_SELFILLUM_MASK_SAMPLER: u32 = 8;
pub const BINDING_ENVMAP_MASK_TEXTURE: u32 = 9;
pub const BINDING_ENVMAP_MASK_SAMPLER: u32 = 10;
/// The environment cubemap. A **cube** view, not a 2D one — the only binding
/// in the set that is, which is why [`TextureRequest`] has to say so.
pub const BINDING_ENVMAP_TEXTURE: u32 = 11;
pub const BINDING_ENVMAP_SAMPLER: u32 = 12;

/// Where the lightmap page is bound, in group **3**.
///
/// Not in the material's group, and that is structural rather than a
/// preference: a lightmapped material spans as many atlas pages as its
/// surfaces needed, so the page is not a property of the material. Valve had
/// the same split — the page is render-context state, set by
/// `IMatRenderContext::BindLightmapPage( lightmapPageID )` once per sort ID,
/// and the shader binds it as the standard texture `TEXTURE_LIGHTMAP`
/// (`lightmappedgeneric_dx9_helper.cpp:583`). Here that is
/// [`Pass::bind_lightmap_page`](super::context::Pass::bind_lightmap_page) and
/// a fourth bind group, which group 3 is free for until skinning lands.
pub const BINDING_LIGHTMAP_TEXTURE: u32 = 0;
pub const BINDING_LIGHTMAP_SAMPLER: u32 = 1;

/// A texture a material of this kind needs, and how to read it.
#[derive(Debug, Clone, Copy)]
pub struct TextureRequest {
    /// The parameter naming it, e.g. `"$basetexture"`.
    pub param: &'static str,
    /// Where the texture goes in the material's bind group. Its sampler goes
    /// in the next binding.
    pub binding: u32,
    pub color_space: ColorSpace,
    /// What shape of view the shader declares here.
    ///
    /// A bind group layout names a `view_dimension`, and binding the wrong
    /// shape is a `wgpu` validation error rather than a wrong picture — so a
    /// request has to carry it, and [`Material::new`](super::material::Material::new)
    /// substitutes the fallback of the matching shape when the `.vmt` names
    /// something else.
    pub dimension: TextureDimension,
}

/// The two view shapes the shader set binds.
///
/// `IShaderShadow` had no equivalent: D3D9 samplers were typed by the *shader*
/// (`sampler` versus `samplerCUBE` in the HLSL) and the runtime just bound
/// whatever texture was in the var. WebGPU types the *layout*, so the shape has
/// to be declared on this side too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureDimension {
    D2,
    Cube,
}

impl TextureDimension {
    pub fn view_dimension(self) -> wgpu::TextureViewDimension {
        match self {
            TextureDimension::D2 => wgpu::TextureViewDimension::D2,
            TextureDimension::Cube => wgpu::TextureViewDimension::Cube,
        }
    }
}

/// Whether a `VertexLitGeneric` `.vmt` is really drawn by the `Phong` shader.
///
/// `WantsPhongShaderInternal` (`vertexlitgeneric_dx9_helper.cpp:70`), which
/// `DrawVertexLitGeneric_DX9` consults before doing anything else and which
/// sends 307 of Portal 2's 1,108 `VertexLitGeneric` materials to
/// `DrawPhong_DX9` instead. `mat_phong` defaults to 1 and there is no video
/// options page here, so `WantsPhongShader`'s outer `mat_phong` test is taken
/// as true and `$forcephong` is redundant.
///
/// **`Phong` is not ported**, so this does not change which code draws the
/// material — it draws here, without specular. What it *is* for is saying so
/// once, at load, instead of leaving a fifth of the game's models quietly
/// wrong with nothing recording why.
pub fn wants_phong(vmt: &Vmt) -> bool {
    let kind = ShaderKind::VertexLitGeneric;
    let defined = |name| {
        vmt.var(name)
            .and_then(|var| var.as_str())
            .is_some_and(|value| !value.is_empty())
    };

    if !param_value(kind, vmt, "$phong").is_some_and(|var| var.as_bool()) {
        return false;
    }
    // A lightwarp is enough on its own: "If there's Phong flag and diffuse
    // warp do Phong".
    if defined("$lightwarptexture") {
        return true;
    }
    // Otherwise a bump map is required — unless the mask is in base alpha,
    // which is the case that exists precisely because there is no normal map.
    // Note the test is `!= 1`, not `== 0`: `$basemapalphaphongmask 2` also
    // skips the bump-map requirement.
    if param_value(kind, vmt, "$basemapalphaphongmask").map(|var| var.as_i32()) != Some(1) {
        return defined("$bumpmap");
    }
    true
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
        // `$basetexture` is bound with `SRGBReadMask( !bShaderSrgbRead )` —
        // sRGB on the PC path — and `$bumpmap` with a plain `EnableTexture`,
        // no sRGB (`lightmappedgeneric_dx9_helper.cpp:731`). A normal map is
        // three signed directions stored as bytes, not a colour; decoding it
        // as one bends every normal towards the surface.
        ShaderKind::LightmappedGeneric => vec![
            TextureRequest {
                param: "$basetexture",
                binding: BINDING_BASE_TEXTURE,
                color_space: ColorSpace::Srgb,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$bumpmap",
                binding: BINDING_BUMP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
        ],
        ShaderKind::UnlitGeneric => {
            vec![TextureRequest {
                param: "$basetexture",
                binding: BINDING_BASE_TEXTURE,
                color_space: base_texture_color_space(kind, vmt),
                dimension: TextureDimension::D2,
            }]
        }
        // `InitVertexLitGeneric_DX9` (`vertexlitgeneric_dx9_helper.cpp:369`),
        // which is one `LoadTexture` per feature with its sRGB flag spelled
        // out. Three of the six are *not* colour and the reasons differ:
        // `$bumpmap` is `LoadBumpMap`, three signed directions stored as
        // bytes; `$selfillummask` and `$envmapmask` are masks, and Valve
        // passes them no flag at all (`:437`, `:463`).
        ShaderKind::VertexLitGeneric => vec![
            TextureRequest {
                param: "$basetexture",
                binding: BINDING_BASE_TEXTURE,
                color_space: base_texture_color_space(kind, vmt),
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$bumpmap",
                binding: BINDING_BUMP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            // `IsSRGBDetailTexture( nMode )` (`BaseVSShader.h:227`): only the
            // three blend modes that put the detail texture *in the albedo*
            // read it as colour. The other ten treat it as a mask or a
            // modulation, where an sRGB decode would bend the curve.
            TextureRequest {
                param: "$detail",
                binding: BINDING_DETAIL_TEXTURE,
                color_space: if is_srgb_detail_texture(detail_blend_mode(vmt)) {
                    ColorSpace::Srgb
                } else {
                    ColorSpace::Linear
                },
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$selfillummask",
                binding: BINDING_SELFILLUM_MASK_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$envmapmask",
                binding: BINDING_ENVMAP_MASK_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            // `LoadCubeMap( info.m_nEnvmap, GetHDRType() == HDR_TYPE_NONE ?
            // TEXTURE_FLAGS_SRGB : 0 )` (`:425`). Portal 2 ships HDR, so the
            // cubemap is linear -- and its *name* gains a `.hdr` on the way to
            // the filesystem, which is `MaterialCache`'s business rather than
            // this table's.
            TextureRequest {
                param: "$envmap",
                binding: BINDING_ENVMAP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::Cube,
            },
        ],
    }
}

/// `$basetexture`'s colour space: sRGB unless `$gammacolorread` says otherwise.
///
/// `vertexlitgeneric_dx9_helper.cpp:784`, shared by both shaders that reach
/// that helper. `$gammacolorread` is not obscure:
/// `CMaterialSystem::CreateDebugMaterials` sets it on the error material itself
/// (`cmaterialsystem.cpp:469`).
fn base_texture_color_space(kind: ShaderKind, vmt: &Vmt) -> ColorSpace {
    if param_value(kind, vmt, "$gammacolorread").is_some_and(|var| var.as_bool()) {
        ColorSpace::Linear
    } else {
        ColorSpace::Srgb
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

    let reference = alpha_test_reference(kind, vmt);

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

/// The alpha a `discard` compares against.
///
/// The fixed-function reference `SetDefaultState` leaves in place
/// (`shadershadowdx8.cpp:233`), unless the material raised it: both helpers
/// override it only when `$alphatestreference` is above zero
/// (`vertexlitgeneric_dx9_helper.cpp:765`,
/// `lightmappedgeneric_dx9_helper.cpp:659`).
fn alpha_test_reference(kind: ShaderKind, vmt: &Vmt) -> f32 {
    param_value(kind, vmt, "$alphatestreference")
        .map(|var| var.as_f32())
        .filter(|value| *value > 0.0)
        .unwrap_or(DEFAULT_ALPHA_TEST_REFERENCE)
}

/// `DETAIL_BLEND_MODE_*` (`stdshaders/BaseVSShader.h:26`), which the shader
/// reads as `TCOMBINE_*` (`common_ps_fxc.h:756`) — two names, one number, and
/// the number is what a `.vmt` writes.
///
/// Declared whole rather than as the two modes Portal 2 uses, because the set
/// is `$detailblendmode`'s content surface area and a number outside it should
/// read as "not implemented" rather than as mode 0.
#[allow(dead_code)]
pub mod detail_blend {
    /// `baseColor.rgb *= lerp( 1, 2 * detail.rgb, blend )`. The original mode.
    pub const MOD2X: i32 = 0;
    pub const ADDITIVE: i32 = 1;
    pub const DETAIL_OVER_BASE: i32 = 2;
    pub const FADE: i32 = 3;
    pub const BASE_OVER_DETAIL: i32 = 4;
    /// Added *after* lighting, in `TextureCombinePostLighting`.
    pub const ADDITIVE_SELFILLUM: i32 = 5;
    pub const ADDITIVE_SELFILLUM_THRESHOLD_FADE: i32 = 6;
    /// Base alpha selects between the detail's `r` and `a` as a mod2x.
    pub const MOD2X_SELECT_TWO_PATTERNS: i32 = 7;
    pub const MULTIPLY: i32 = 8;
    pub const MASK_BASE_BY_DETAIL_ALPHA: i32 = 9;
    pub const SSBUMP_BUMP: i32 = 10;
    pub const SSBUMP_NOBUMP: i32 = 11;
    /// Not a mode a `.vmt` writes: what the shader is told when there is no
    /// detail texture at all.
    pub const NONE: i32 = 12;
}

/// `$detailblendmode`, defaulting to 0.
fn detail_blend_mode(vmt: &Vmt) -> i32 {
    param_value(ShaderKind::VertexLitGeneric, vmt, "$detailblendmode")
        .map(|var| var.as_i32())
        .unwrap_or(detail_blend::MOD2X)
}

/// `IsSRGBDetailTexture( nMode )` (`stdshaders/BaseVSShader.h:227`).
///
/// Only the three modes that composite the detail texture into the albedo read
/// it as colour; the rest use it as a mask or a multiplier, where an sRGB
/// decode would bend a curve that was authored linear.
fn is_srgb_detail_texture(mode: i32) -> bool {
    matches!(
        mode,
        detail_blend::DETAIL_OVER_BASE | detail_blend::FADE | detail_blend::BASE_OVER_DETAIL
    )
}

/// How a material is lit, and therefore what the world builder has to allocate
/// for the surfaces that wear it.
///
/// `MATERIAL_VAR2_LIGHTING_LIGHTMAP` and
/// `MATERIAL_VAR2_LIGHTING_BUMPED_LIGHTMAP`, which reach the engine as
/// `IMaterial::GetPropertyFlag( MATERIAL_PROPERTY_NEEDS_LIGHTMAP )` and
/// `..._NEEDS_BUMPED_LIGHTMAPS` (`cmaterial.cpp:2946`). Those two answers are
/// what `RegisterLightmappedSurface` (`gl_matsysiface.cpp:216`) asks before it
/// decides how wide a block to reserve in the atlas, so this is a
/// material-system property with an engine-side consequence, not a rendering
/// detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lighting {
    /// Nothing baked. The surface binds the white page.
    None,
    /// One lightmap block per surface.
    Lightmap,
    /// Four blocks per surface: the flat map and one per basis vector.
    BumpedLightmap,
}

impl Lighting {
    /// How many lightmap blocks wide a surface's allocation is.
    pub fn blocks(self) -> u32 {
        match self {
            Lighting::BumpedLightmap => super::lightmap::BUMP_BLOCKS,
            _ => 1,
        }
    }

    pub fn needs_lightmap(self) -> bool {
        !matches!(self, Lighting::None)
    }
}

/// How a `.vmt` of this shader is lit.
///
/// `InitParamsLightmappedGeneric_DX9` (`lightmappedgeneric_dx9_helper.cpp:168`)
/// in two lines:
///
/// ```text
/// SET_FLAGS2( MATERIAL_VAR2_LIGHTING_LIGHTMAP );
/// bool bShouldUseBump = g_pConfig->UseBumpmapping() || $forcebump;
/// if ( bShouldUseBump && $bumpmap is defined && $nodiffusebumplighting == 0 )
///     SET_FLAGS2( MATERIAL_VAR2_LIGHTING_BUMPED_LIGHTMAP );
/// ```
///
/// `g_pConfig->UseBumpmapping()` is `mat_bumpmap`, a video option that does not
/// exist here; it defaults on, so it is taken as on, which also makes
/// `$forcebump` redundant. The parameter is still declared because content
/// sets it and a table that omits it would send the next reader back to the
/// C++.
///
/// **A `.vmt` that names a `$bumpmap` therefore changes how the `.bsp`'s
/// lighting lump is read**, four bytes per luxel at a time. That coupling is
/// Valve's, and it is the reason this answer lives on the material rather than
/// being derived where it is used.
pub fn lighting(kind: ShaderKind, vmt: &Vmt) -> Lighting {
    match kind {
        // `MATERIAL_VAR2_LIGHTING_VERTEX_LIT` (`vertexlitgeneric_dx9_helper.cpp:202`),
        // which `RegisterLightmappedSurface` treats as "no lightmap": a model
        // carries its baked light in its vertices, not in the atlas.
        ShaderKind::UnlitGeneric | ShaderKind::VertexLitGeneric => Lighting::None,
        ShaderKind::LightmappedGeneric => {
            let has_bump = vmt
                .var("$bumpmap")
                .and_then(|var| var.as_str())
                .is_some_and(|name| !name.is_empty());
            let no_diffuse_bump =
                param_value(kind, vmt, "$nodiffusebumplighting").is_some_and(|var| var.as_bool());
            if has_bump && !no_diffuse_bump {
                Lighting::BumpedLightmap
            } else {
                Lighting::Lightmap
            }
        }
    }
}

/// Flags in [`LightmappedUniforms::flags`].
#[allow(dead_code)]
pub struct LightmappedFlags;

impl LightmappedFlags {
    /// `VERTEXCOLOR`, a static combo of `lightmappedgeneric_vs20.fxc`. Set by
    /// `$vertexcolor`.
    pub const VERTEX_COLOR: u32 = 1 << 0;
    /// Alpha testing, fixed-function state in D3D9.
    pub const ALPHA_TEST: u32 = 1 << 1;
    /// `SHADER_FOGMODE_DISABLED`. Set by `$nofog`.
    pub const NO_FOG: u32 = 1 << 2;
    /// `BUMPMAP` plus `MATERIAL_VAR2_LIGHTING_BUMPED_LIGHTMAP` — radiosity
    /// normal mapping, sampling the three directional lightmap blocks instead
    /// of the flat one. [`Lighting::BumpedLightmap`].
    pub const BUMPED_LIGHTMAP: u32 = 1 << 3;
}

/// `LightmappedGeneric`'s material block — group 1, binding 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct LightmappedUniforms {
    /// `$basetexturetransform`, two rows dotted against `(u, v, 0, 1)`.
    pub base_texture_transform: [[f32; 4]; 2],
    /// `$bumptransform`, the same shape. Applied to the *base* texture
    /// coordinate, which is what `lightmappedgeneric_vs20.fxc:205` does — the
    /// bump map shares texture space with the albedo.
    pub bump_transform: [[f32; 4]; 2],
    /// `$alphatestreference`, or the fixed-function default of 0.7.
    pub alpha_test_reference: f32,
    /// [`LightmappedFlags`].
    pub flags: u32,
    pub _padding: [u32; 2],
}

/// Builds the material block for a `.vmt`.
pub fn lightmapped_uniforms(vmt: &Vmt) -> LightmappedUniforms {
    let kind = ShaderKind::LightmappedGeneric;
    let transform = |name| {
        param_value(kind, vmt, name)
            .map(|var| var.as_matrix())
            .unwrap_or(super::var::IDENTITY)
    };
    let base = transform("$basetexturetransform");
    let bump = transform("$bumptransform");

    let mut flags = 0;
    if vmt.flags.contains(MaterialFlags::VERTEXCOLOR) {
        flags |= LightmappedFlags::VERTEX_COLOR;
    }
    if vmt.flags.contains(MaterialFlags::ALPHATEST) {
        flags |= LightmappedFlags::ALPHA_TEST;
    }
    if vmt.flags.contains(MaterialFlags::NOFOG) {
        flags |= LightmappedFlags::NO_FOG;
    }
    if lighting(kind, vmt) == Lighting::BumpedLightmap {
        flags |= LightmappedFlags::BUMPED_LIGHTMAP;
    }

    LightmappedUniforms {
        base_texture_transform: [base[0], base[1]],
        bump_transform: [bump[0], bump[1]],
        alpha_test_reference: alpha_test_reference(kind, vmt),
        flags,
        _padding: [0; 2],
    }
}

/// Flags in [`VertexLitUniforms::flags`]. Bucket 2 of the combo split: what
/// used to be a static shader variant and is now an `if` on a uniform.
#[allow(dead_code)]
pub struct VertexLitFlags;

impl VertexLitFlags {
    /// Alpha testing, fixed-function state in D3D9.
    pub const ALPHA_TEST: u32 = 1 << 0;
    /// `SHADER_FOGMODE_DISABLED`. Set by `$nofog`.
    pub const NO_FOG: u32 = 1 << 1;
    /// `BUMPMAP`: the material has a `$bumpmap`, so lighting is per pixel
    /// against a normal read from it rather than per vertex. This is the axis
    /// that picked between `vertexlit_and_unlit_generic_ps2x.fxc` and the
    /// `_bump_` file of the same name; here it is one branch.
    pub const BUMPMAP: u32 = 1 << 2;
    /// `CUBEMAP`: the material has a usable `$envmap`.
    pub const ENVMAP: u32 = 1 << 3;
    /// `ENVMAPMASK`: `$envmapmask` scales the reflection.
    pub const ENVMAP_MASK: u32 = 1 << 4;
    /// `BASEALPHAENVMAPMASK`: the base texture's alpha scales it instead.
    pub const BASE_ALPHA_ENVMAP_MASK: u32 = 1 << 5;
    /// `NORMALMAPALPHAENVMAPMASK`: the *normal map's* alpha does.
    pub const NORMAL_ALPHA_ENVMAP_MASK: u32 = 1 << 6;
    /// `ENVMAPFRESNEL`: the reflection is scaled by a fresnel term.
    pub const ENVMAP_FRESNEL: u32 = 1 << 7;
    /// `SELFILLUM`: part of the albedo is emitted rather than lit.
    pub const SELFILLUM: u32 = 1 << 8;
    /// `$selfillummask` is bound, so the mask comes from it rather than from
    /// base alpha. `MATERIAL_VAR2_SELFILLUMMASK`.
    pub const SELFILLUM_MASK: u32 = 1 << 9;
    /// `DETAILTEXTURE`: a `$detail` texture is bound.
    pub const DETAIL: u32 = 1 << 10;
    /// `HALFLAMBERT`: the diffuse term is `(N·L * 0.5 + 0.5)²` rather than
    /// `saturate( N·L )`. `$halflambert`.
    pub const HALF_LAMBERT: u32 = 1 << 11;
    /// `$blendtintbybasealpha`: base alpha decides how much of `$color`
    /// reaches the lighting.
    pub const BLEND_TINT_BY_BASE_ALPHA: u32 = 1 << 12;
}

/// `VertexLitGeneric`'s material block — group 1, binding 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VertexLitUniforms {
    /// `$basetexturetransform`, two rows dotted against `(u, v, 0, 1)`.
    pub base_texture_transform: [[f32; 4]; 2],
    /// `$bumptransform`. Its own transform, unlike `LightmappedGeneric`'s,
    /// because `vertexlit_and_unlit_generic_bump_vs20.fxc:255` gives the bump
    /// coordinate a separate `cBumpTexCoordTransform`.
    pub bump_transform: [[f32; 4]; 2],
    /// `cDetailTexCoordTransform`, which is `$detailtexturetransform` scaled by
    /// `$detailscale` — `SetVertexShaderTextureScaledTransform`
    /// (`BaseVSShader.cpp:294`). The scale is folded in here rather than in the
    /// shader, exactly as Valve folds it.
    pub detail_transform: [[f32; 4]; 2],
    /// `$selfillumtint` in `rgb`, `$selfillummaskscale` in `w`.
    pub selfillum_tint: [f32; 4],
    /// `$envmaptint` in `rgb`, `$envmapcontrast` in `w`.
    pub envmap_tint: [f32; 4],
    /// `$envmapsaturation` in `x`, `$envmapfresnel` in `y`, and
    /// `$detailtint`'s luminance-neutral counterpart is elsewhere: `z` and `w`
    /// are the fresnel range's scale and bias, derived from
    /// `$envmapfresnelminmaxexp` by `SetupFresnelParams`.
    pub envmap_params: [f32; 4],
    /// `$envmapfresnelminmaxexp`'s exponent in `x`, and the
    /// `$basealphaenvmapmask` scale, bias and exponent in `yzw` — the three
    /// numbers `g_FresnelConstants` and `g_DistanceAlphaParams.zw` carry
    /// between them (`vertexlit_and_unlit_generic_ps2x.fxc:152,179`).
    pub fresnel_params: [f32; 4],
    /// `g_DetailTint` in `rgb`, `$detailblendfactor` in `w`.
    pub detail_tint: [f32; 4],
    /// `$alphatestreference`, or the fixed-function default of 0.7.
    pub alpha_test_reference: f32,
    /// `$detailblendmode`. Not a flag bit because it is a *number* the shader
    /// switches on — §7.3's bucket 2 with more than two values.
    pub detail_blend_mode: i32,
    /// [`VertexLitFlags`].
    pub flags: u32,
    pub _padding: u32,
}

/// Builds the material block for a `.vmt`.
pub fn vertex_lit_uniforms(vmt: &Vmt) -> VertexLitUniforms {
    let kind = ShaderKind::VertexLitGeneric;
    let value = |name| param_value(kind, vmt, name);
    let transform = |name| {
        value(name)
            .map(|var| var.as_matrix())
            .unwrap_or(super::var::IDENTITY)
    };
    let defined = |name| {
        vmt.var(name)
            .and_then(|var| var.as_str())
            .is_some_and(|value| !value.is_empty())
    };
    let float = |name, default| init_float(vmt, name, default);

    let base = transform("$basetexturetransform");
    let bump = transform("$bumptransform");

    // `SetVertexShaderTextureScaledTransform` (`BaseVSShader.cpp:294`)
    // multiplies the whole transform — translation included — by
    // `$detailscale`, which is why a detail texture tiles about its origin
    // rather than about the surface's texture origin.
    let detail_scale = float("$detailscale", 4.0);
    let detail = transform("$detailtexturetransform");
    let detail = [
        [
            detail[0][0] * detail_scale,
            detail[0][1] * detail_scale,
            detail[0][2] * detail_scale,
            detail[0][3] * detail_scale,
        ],
        [
            detail[1][0] * detail_scale,
            detail[1][1] * detail_scale,
            detail[1][2] * detail_scale,
            detail[1][3] * detail_scale,
        ],
    ];

    let has_bump = defined("$bumpmap");
    let has_envmap = envmap_name(vmt).is_some();
    let has_detail = defined("$detail");
    // `InitVertexLitGeneric_DX9:394` clears `MATERIAL_VAR_SELFILLUM` when the
    // base texture has no alpha channel to hold the mask, unless a
    // `$selfillummask` supplies one. The texture is not available here, so the
    // flag is taken at face value and a self-illuminating material with an
    // opaque base texture reads its alpha as 1 — which is what the shipped
    // engine would have drawn had the flag survived, and is fully emissive
    // rather than subtly wrong.
    let has_selfillum = vmt.flags.contains(MaterialFlags::SELFILLUM);
    let has_selfillum_mask = has_selfillum && defined("$selfillummask");

    let mut flags = 0;
    if vmt.flags.contains(MaterialFlags::ALPHATEST) {
        // "Don't alpha test if the alpha channel is used for other purposes"
        // (`vertexlitgeneric_dx9_helper.cpp:417`): `$selfillum` without a mask
        // texture, and `$basealphaenvmapmask`, both claim base alpha.
        let alpha_is_spoken_for = (has_selfillum && !has_selfillum_mask)
            || vmt.flags.contains(MaterialFlags::BASEALPHAENVMAPMASK);
        if !alpha_is_spoken_for {
            flags |= VertexLitFlags::ALPHA_TEST;
        }
    }
    if vmt.flags.contains(MaterialFlags::NOFOG) {
        flags |= VertexLitFlags::NO_FOG;
    }
    if has_bump {
        flags |= VertexLitFlags::BUMPMAP;
    }
    if has_envmap {
        flags |= VertexLitFlags::ENVMAP;

        // `InitParamsVertexLitGeneric_DX9:255` resolves the three envmap masks
        // against each other, in this order, because they all want the same
        // scalar and two of them want the same alpha channel:
        //
        //   - `$normalmapalphaenvmapmask` wins and undefines `$envmapmask`.
        //   - a `$bumpmap` plus `$basealphaenvmapmask` without it is a content
        //     error Valve warns about and answers by dropping the *envmap*.
        //   - a `$bumpmap` plus an `$envmapmask` likewise.
        let normal_alpha = vmt.flags.contains(MaterialFlags::NORMALMAPALPHAENVMAPMASK);
        if normal_alpha && has_bump {
            flags |= VertexLitFlags::NORMAL_ALPHA_ENVMAP_MASK;
        } else if defined("$envmapmask") && !has_bump {
            flags |= VertexLitFlags::ENVMAP_MASK;
        } else if vmt.flags.contains(MaterialFlags::BASEALPHAENVMAPMASK) && !has_bump {
            flags |= VertexLitFlags::BASE_ALPHA_ENVMAP_MASK;
        }

        // `IsBoolSet` (`BaseVSShader.h:346`) is `GetIntValue() != 0`, which
        // *truncates*: `$envmapfresnel 0.5` is off in Valve's engine even
        // though the parameter is declared a float. `as_bool` is that
        // truncation. Every one of the 30 Portal 2 materials that set this
        // writes "1", so the two readings agree on shipped content and would
        // not on a fractional value.
        if value("$envmapfresnel").is_some_and(|var| var.as_bool()) {
            flags |= VertexLitFlags::ENVMAP_FRESNEL;
        }
    }
    if has_selfillum {
        flags |= VertexLitFlags::SELFILLUM;
    }
    if has_selfillum_mask {
        flags |= VertexLitFlags::SELFILLUM_MASK;
    }
    if has_detail {
        flags |= VertexLitFlags::DETAIL;
    }
    // **Restored from the flag, against the CS:GO tree this port is derived
    // from.** `vertexlitgeneric_dx9_helper.cpp:679` reads
    //
    //     //bool bHalfLambert = IS_FLAG_SET( MATERIAL_VAR_HALFLAMBERT );
    //     // Disabling half-lambert for CSGO (not compatible with CSM's,
    //     // causes bad shadow aliasing).
    //     bool bHalfLambert = false;
    //
    // — the commented-out line is the Portal 2 behaviour and the constant below
    // it is a CS:GO change made for cascaded shadow maps, which Portal 2 does
    // not have and this port does not implement. `PORTING.md`'s standing
    // warning about CS:GO-shaped defaults in shared systems is exactly this.
    if vmt.flags.contains(MaterialFlags::HALFLAMBERT) {
        flags |= VertexLitFlags::HALF_LAMBERT;
    }
    if value("$blendtintbybasealpha").is_some_and(|var| var.as_bool()) {
        flags |= VertexLitFlags::BLEND_TINT_BY_BASE_ALPHA;
    }

    // All three are `SetVecValue( 1, 1, 1 )` in `InitParamsVertexLitGeneric_DX9`
    // (`:139`, `:145`, `:158`), which is *not* what their declared `Color` type
    // would give them.
    let selfillum_tint = init_vec(vmt, "$selfillumtint", [1.0, 1.0, 1.0, 0.0]);
    let envmap_tint = init_vec(vmt, "$envmaptint", [1.0, 1.0, 1.0, 0.0]);
    let detail_tint = init_vec(vmt, "$detailtint", [1.0, 1.0, 1.0, 0.0]);

    // `$envmapfresnelminmaxexp` and `$basealphaenvmapmaskminmaxexp` are both
    // (min, max, exp) triples that the shader applies as
    // `scale * pow( x, exp ) + bias` — so the scale is `max - min` and the bias
    // is `min`. `$basealphaenvmapmask`'s default of `[1 0 1]` therefore means
    // scale -1, bias 1, exponent 1, which is `1 - baseColor.a`: Valve's own
    // comment calls that "the legacy behavior", and it is *inverted* relative
    // to what the parameter's name suggests.
    let fresnel = init_vec(vmt, "$envmapfresnelminmaxexp", [0.0, 1.0, 2.0, 0.0]);
    let base_alpha_mask = init_vec(vmt, "$basealphaenvmapmaskminmaxexp", [1.0, 0.0, 1.0, 0.0]);

    VertexLitUniforms {
        base_texture_transform: [base[0], base[1]],
        bump_transform: [bump[0], bump[1]],
        detail_transform: detail,
        selfillum_tint: [
            selfillum_tint[0],
            selfillum_tint[1],
            selfillum_tint[2],
            float("$selfillummaskscale", 1.0),
        ],
        envmap_tint: [
            envmap_tint[0],
            envmap_tint[1],
            envmap_tint[2],
            float("$envmapcontrast", 0.0),
        ],
        envmap_params: [
            float("$envmapsaturation", 1.0),
            float("$envmapfresnel", 0.0),
            fresnel[1] - fresnel[0],
            fresnel[0],
        ],
        fresnel_params: [
            fresnel[2],
            base_alpha_mask[1] - base_alpha_mask[0],
            base_alpha_mask[0],
            base_alpha_mask[2],
        ],
        detail_tint: [
            detail_tint[0],
            detail_tint[1],
            detail_tint[2],
            float("$detailblendfactor", 1.0),
        ],
        alpha_test_reference: alpha_test_reference(kind, vmt),
        detail_blend_mode: if has_detail {
            detail_blend_mode(vmt)
        } else {
            detail_blend::NONE
        },
        flags,
        _padding: 0,
    }
}

/// The `.vtf` a `$envmap` names, if it names one this port can load.
///
/// **`env_cubemap` is not a texture name**, and that is the finding this
/// function exists to record. `CShaderSystem::LoadCubeMap`
/// (`shadersystem.cpp:1840`) special-cases the literal string: it sets the var
/// to `(ITexture *)-1`, sets `MATERIAL_VAR2_USES_ENV_CUBEMAP`, and loads
/// nothing. The cubemap then arrives *per draw*, from the render instance —
/// `instance.m_pEnvCubemap`, falling back to
/// `m_StdTextureHandles[TEXTURE_LOCAL_ENV_CUBEMAP]`
/// (`shaderapidx8.cpp:8370`) — because which cubemap a model reflects depends
/// on where the model is standing, not on its material.
///
/// 78 of Portal 2's 801 non-phong `VertexLitGeneric` materials say
/// `env_cubemap`. They get the fallback cubemap until the `.bsp`'s embedded
/// cubemaps are readable, which needs the pak lump mounted; at that point this
/// becomes render-context state alongside the lightmap page rather than a
/// material texture, and *that* is the trigger to revisit.
///
/// The other half of this function is the `.hdr` suffix. `LoadCubeMap` appends
/// it whenever HDR is on (`shadersystem.cpp:1855`) and `CTexture` falls back to
/// the unsuffixed name when the suffixed one is missing
/// (`ctexture.cpp:3882`) — so `$envmap "metal/foo"` means `metal/foo.hdr.vtf`
/// **or** `metal/foo.vtf`, in that order. Portal 2 ships exactly one
/// `.hdr.vtf`, so dropping the rule would look correct on nearly every
/// material and load the wrong file for that one.
pub fn envmap_name(vmt: &Vmt) -> Option<&str> {
    let name = vmt.var("$envmap").and_then(|var| var.as_str())?;
    if name.is_empty() || name.eq_ignore_ascii_case("env_cubemap") {
        return None;
    }
    Some(name)
}

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
    //
    // **Not `LightmappedGeneric`, and that asymmetry is the original's.**
    // `$multiply` is handled by the shared helper, which is the one both
    // `UnlitGeneric` and `VertexLitGeneric` reach, and by
    // `CBaseShader::SetInitialShadowState` not at all
    // (`shaderlib/BaseShader.cpp:183` has no `MATERIAL_VAR_MULTIPLY` case), so
    // a `LightmappedGeneric` material that sets `$multiply` gets ordinary
    // blending in Valve's engine too. Content does not set it on world
    // surfaces; reproducing the gap costs nothing and diverging from it would
    // be a silent change to how a wall blends.
    if kind != ShaderKind::LightmappedGeneric && flags.contains(MaterialFlags::MULTIPLY) {
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
        assert_eq!(
            ShaderKind::from_name("lightmappedgeneric"),
            Some(ShaderKind::LightmappedGeneric)
        );
        assert_eq!(
            ShaderKind::from_name("vertexlitgeneric"),
            Some(ShaderKind::VertexLitGeneric)
        );
        // A fallback name is not a shader: that mechanism is deleted.
        assert_eq!(ShaderKind::from_name("UnlitGeneric_dx9"), None);
        assert_eq!(ShaderKind::from_name("LightmappedGeneric_dx9"), None);
        assert_eq!(ShaderKind::from_name("VertexLitGeneric_dx9"), None);
        // Phong is a real Valve shader name and is deliberately not ported:
        // the materials that want it reach `VertexLitGeneric` instead. See
        // `wants_phong`.
        assert_eq!(ShaderKind::from_name("Phong"), None);
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
        // Three 2x4 transforms, five vec4s, then four words.
        assert_eq!(size_of::<VertexLitUniforms>(), 3 * 32 + 5 * 16 + 16);
        assert_eq!(size_of::<VertexLitUniforms>() % 16, 0);
    }

    // ---------------------------------------------------------------------
    // VertexLitGeneric
    // ---------------------------------------------------------------------

    /// A `.vmt` naming `VertexLitGeneric` rather than the module's default.
    fn model_vmt(body: &str) -> Vmt {
        let text = format!("\"VertexLitGeneric\" {{ {body} }}");
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        Vmt::from_keyvalues("test.vmt", &document).expect("a shader block")
    }

    #[test]
    fn phong_materials_are_recognised_but_not_ported() {
        // `WantsPhongShaderInternal` (`vertexlitgeneric_dx9_helper.cpp:70`),
        // which decides whether a `VertexLitGeneric` `.vmt` is really drawn by
        // `Phong` — 307 of Portal 2's 1,108 of them.

        // `$phong` alone is not enough: there has to be a mask to use.
        assert!(!wants_phong(&model_vmt(r#""$phong" "1""#)));
        // A bump map is the usual one.
        assert!(wants_phong(&model_vmt(r#""$phong" "1" "$bumpmap" "x""#)));
        // A lightwarp short-circuits before the bump-map test.
        assert!(wants_phong(&model_vmt(
            r#""$phong" "1" "$lightwarptexture" "x""#
        )));
        // `$basemapalphaphongmask 1` exists precisely because there is no
        // normal map, so it replaces the requirement rather than adding to it.
        assert!(wants_phong(&model_vmt(
            r#""$phong" "1" "$basemapalphaphongmask" "1""#
        )));
        // The test is `!= 1`, not `== 0`: any other value still needs a bump
        // map.
        assert!(!wants_phong(&model_vmt(
            r#""$phong" "1" "$basemapalphaphongmask" "2""#
        )));
        // And without `$phong` nothing else matters.
        assert!(!wants_phong(&model_vmt(r#""$bumpmap" "x""#)));
        assert!(!wants_phong(&model_vmt(r#""$phong" "0" "$bumpmap" "x""#)));
    }

    #[test]
    fn env_cubemap_is_not_a_texture_name() {
        // `CShaderSystem::LoadCubeMap` (`shadersystem.cpp:1840`) special-cases
        // the literal string and loads nothing; the cubemap arrives per draw
        // from the render instance instead. 78 of Portal 2's non-phong
        // `VertexLitGeneric` materials say it, so treating it as a filename
        // would be 78 warnings and 78 checkerboards.
        assert_eq!(envmap_name(&model_vmt(r#""$envmap" "env_cubemap""#)), None);
        assert_eq!(envmap_name(&model_vmt(r#""$envmap" "ENV_CUBEMAP""#)), None);
        assert_eq!(envmap_name(&model_vmt(r#""$envmap" """#)), None);
        assert_eq!(envmap_name(&model_vmt("")), None);
        assert_eq!(
            envmap_name(&model_vmt(r#""$envmap" "metal/black_wall_envmap_002a""#)),
            Some("metal/black_wall_envmap_002a")
        );
    }

    #[test]
    fn shader_supplied_defaults_beat_type_defaults() {
        // The trap `init_float` exists for. `param_value` answers an undefined
        // float with 0 — `InitShaderParameters`' answer — but the shader's own
        // `InitParams` block ran first and wrote 4. Reaching for `param_value`
        // and appending `.unwrap_or( 4.0 )` compiles, reads correctly, and is
        // dead code.
        let vmt = model_vmt("");
        assert_eq!(
            param_value(ShaderKind::VertexLitGeneric, &vmt, "$detailscale").map(|var| var.as_f32()),
            Some(0.0),
            "the type default, which is the one that is wrong here"
        );
        assert_eq!(init_float(&vmt, "$detailscale", 4.0), 4.0);
        // And an explicit value still wins.
        assert_eq!(
            init_float(&model_vmt(r#""$detailscale" "8""#), "$detailscale", 4.0),
            8.0
        );
    }

    #[test]
    fn the_detail_scale_is_folded_into_the_detail_transform() {
        // `SetVertexShaderTextureScaledTransform` (`BaseVSShader.cpp:294`)
        // multiplies the whole transform by `$detailscale`, translation
        // included. A detail texture therefore tiles about the *texture*
        // origin, not the surface's.
        let uniforms = vertex_lit_uniforms(&model_vmt(r#""$detail" "x""#));
        assert_eq!(uniforms.detail_transform[0], [4.0, 0.0, 0.0, 0.0]);
        assert_eq!(uniforms.detail_transform[1], [0.0, 4.0, 0.0, 0.0]);

        let uniforms = vertex_lit_uniforms(&model_vmt(
            r#""$detail" "x" "$detailscale" "2"
               "$detailtexturetransform" "center 0 0 scale 1 1 rotate 0 translate .5 0""#,
        ));
        assert_eq!(uniforms.detail_transform[0][0], 2.0);
        assert_eq!(uniforms.detail_transform[0][3], 1.0, "the translation too");
    }

    #[test]
    fn half_lambert_comes_back_from_the_flag() {
        // The CS:GO divergence this port reverses:
        // `vertexlitgeneric_dx9_helper.cpp:679` hard-codes `bHalfLambert =
        // false` over a commented-out read of `MATERIAL_VAR_HALFLAMBERT`.
        let uniforms = vertex_lit_uniforms(&model_vmt(r#""$halflambert" "1""#));
        assert_eq!(
            uniforms.flags & VertexLitFlags::HALF_LAMBERT,
            VertexLitFlags::HALF_LAMBERT
        );
        assert_eq!(vertex_lit_uniforms(&model_vmt("")).flags, 0);
    }

    #[test]
    fn the_three_envmap_masks_resolve_against_each_other() {
        // `InitParamsVertexLitGeneric_DX9:255`. All three want the same
        // scalar and two want the same alpha channel, so the order matters.
        let flags = |body: &str| vertex_lit_uniforms(&model_vmt(body)).flags;
        let envmap = r#""$envmap" "cubemaps/x""#;

        // Normal-map alpha wins, and undefines `$envmapmask`.
        let f = flags(&format!(
            r#"{envmap} "$bumpmap" "b" "$normalmapalphaenvmapmask" "1" "$envmapmask" "m""#
        ));
        assert_eq!(
            f & VertexLitFlags::NORMAL_ALPHA_ENVMAP_MASK,
            VertexLitFlags::NORMAL_ALPHA_ENVMAP_MASK
        );
        assert_eq!(f & VertexLitFlags::ENVMAP_MASK, 0);

        // An `$envmapmask` with no bump map is honoured.
        let f = flags(&format!(r#"{envmap} "$envmapmask" "m""#));
        assert_eq!(f & VertexLitFlags::ENVMAP_MASK, VertexLitFlags::ENVMAP_MASK);

        // A bump map plus `$basealphaenvmapmask` and no
        // `$normalmapalphaenvmapmask` is the content error Valve warns about;
        // neither mask applies.
        let f = flags(&format!(
            r#"{envmap} "$bumpmap" "b" "$basealphaenvmapmask" "1""#
        ));
        assert_eq!(f & VertexLitFlags::BASE_ALPHA_ENVMAP_MASK, 0);

        // Without an `$envmap` at all, no mask is set whatever content says.
        let f = flags(r#""$envmapmask" "m" "$basealphaenvmapmask" "1""#);
        assert_eq!(f & VertexLitFlags::ENVMAP, 0);
        assert_eq!(f & VertexLitFlags::ENVMAP_MASK, 0);
        assert_eq!(f & VertexLitFlags::BASE_ALPHA_ENVMAP_MASK, 0);
    }

    #[test]
    fn alpha_testing_is_dropped_when_base_alpha_is_spoken_for() {
        // "Don't alpha test if the alpha channel is used for other purposes"
        // (`vertexlitgeneric_dx9_helper.cpp:417`). Both of these claim base
        // alpha, and testing against it as well would discard exactly the
        // texels the feature is about.
        let flag =
            |body: &str| vertex_lit_uniforms(&model_vmt(body)).flags & VertexLitFlags::ALPHA_TEST;

        assert_eq!(flag(r#""$alphatest" "1""#), VertexLitFlags::ALPHA_TEST);
        assert_eq!(flag(r#""$alphatest" "1" "$selfillum" "1""#), 0);
        assert_eq!(flag(r#""$alphatest" "1" "$basealphaenvmapmask" "1""#), 0);
        // A `$selfillummask` frees base alpha again, which is what
        // `MATERIAL_VAR2_SELFILLUMMASK` is for.
        assert_eq!(
            flag(r#""$alphatest" "1" "$selfillum" "1" "$selfillummask" "m""#),
            VertexLitFlags::ALPHA_TEST
        );
    }

    #[test]
    fn multiply_reaches_every_shader_that_uses_the_shared_helper() {
        // `$multiply` is handled at the end of `vertexlitgeneric_dx9_helper`'s
        // shadow block, which `UnlitGeneric` *and* `VertexLitGeneric` reach and
        // `LightmappedGeneric` does not.
        let body = r#""$multiply" "1""#;
        let unlit = format!(r#""UnlitGeneric" {{ {body} }}"#);
        let model = format!(r#""VertexLitGeneric" {{ {body} }}"#);
        let world = format!(r#""LightmappedGeneric" {{ {body} }}"#);
        let parse = |text: &str, name: &str| {
            let document = keyvalues::parse(name, text).unwrap();
            Vmt::from_keyvalues(name, &document).unwrap()
        };

        for (kind, text) in [
            (ShaderKind::UnlitGeneric, &unlit),
            (ShaderKind::VertexLitGeneric, &model),
        ] {
            let state = render_state(kind, &parse(text, "m.vmt"), None);
            assert_eq!(state.blend, BlendMode::Multiply, "{}", kind.name());
        }
        let state = render_state(
            ShaderKind::LightmappedGeneric,
            &parse(&world, "w.vmt"),
            None,
        );
        assert_eq!(
            state.blend,
            BlendMode::None,
            "a world surface ignores $multiply in Valve's engine too"
        );
    }

    #[test]
    fn only_the_albedo_detail_modes_read_srgb() {
        // `IsSRGBDetailTexture` (`BaseVSShader.h:227`). The other ten modes use
        // the texture as a mask or a multiplier, where an sRGB decode bends a
        // curve that was authored linear.
        let space = |mode: i32| {
            let body = format!(r#""$detail" "d" "$detailblendmode" "{mode}""#);
            texture_requests(ShaderKind::VertexLitGeneric, &model_vmt(&body))
                .into_iter()
                .find(|request| request.param == "$detail")
                .expect("the detail request is declared")
                .color_space
        };
        for mode in [
            detail_blend::DETAIL_OVER_BASE,
            detail_blend::FADE,
            detail_blend::BASE_OVER_DETAIL,
        ] {
            assert_eq!(space(mode), ColorSpace::Srgb, "mode {mode}");
        }
        for mode in [
            detail_blend::MOD2X,
            detail_blend::ADDITIVE,
            detail_blend::MOD2X_SELECT_TWO_PATTERNS,
            detail_blend::MULTIPLY,
        ] {
            assert_eq!(space(mode), ColorSpace::Linear, "mode {mode}");
        }
    }

    #[test]
    fn the_envmap_is_the_only_cube_binding() {
        // A bind group layout names a view dimension, so this is what keeps
        // `Material::new` from binding a 2D texture into a cube slot — a
        // validation error rather than a wrong picture.
        let requests = texture_requests(ShaderKind::VertexLitGeneric, &model_vmt(""));
        for request in &requests {
            let expected = if request.param == "$envmap" {
                TextureDimension::Cube
            } else {
                TextureDimension::D2
            };
            assert_eq!(request.dimension, expected, "{}", request.param);
        }
        assert!(requests.iter().any(|r| r.param == "$envmap"));
    }

    #[test]
    fn a_model_material_reserves_no_lightmap() {
        // `MATERIAL_VAR2_LIGHTING_VERTEX_LIT`, which
        // `RegisterLightmappedSurface` reads as "no atlas block": a model
        // carries its baked light in its vertices.
        assert_eq!(
            lighting(
                ShaderKind::VertexLitGeneric,
                &model_vmt(r#""$bumpmap" "b""#)
            ),
            Lighting::None
        );
        assert_eq!(
            ShaderKind::VertexLitGeneric.lighting_binding(),
            Some(LightingBinding::ModelLighting)
        );
        assert_eq!(
            ShaderKind::LightmappedGeneric.lighting_binding(),
            Some(LightingBinding::LightmapPage)
        );
        assert_eq!(ShaderKind::UnlitGeneric.lighting_binding(), None);
    }
}
