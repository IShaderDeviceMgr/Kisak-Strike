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

/// A shader that draws — one WGSL module, one group-1 layout, one parameter
/// table.
///
/// Replaces `CShaderSystem::FindShader`'s dictionary
/// (`shadersystem.cpp:1290`), which looked a name up in a `CUtlDict` populated
/// by whichever `shaderapi.so` had been `dlopen`ed. There is no registration
/// step here and no way to fail to be registered.
///
/// **Not quite "a shader a `.vmt` can name", and the gap is real in the
/// original too.** Five of these are names content writes;
/// [`Phong`](ShaderKind::Phong) is not a name at all, because
/// `DrawVertexLitGeneric_DX9` redirects to it after the name lookup has
/// already happened. [`from_name`](ShaderKind::from_name) answers the first
/// question and [`resolve`](ShaderKind::resolve) answers "what draws this",
/// which is the one almost every caller means.
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

    /// **The same shader as [`LightmappedGeneric`](ShaderKind::LightmappedGeneric)**,
    /// under the name content uses when it means "two base textures blended by
    /// the vertex alpha". Terrain, almost exclusively: 937 of Portal 2's 1,181
    /// displacement faces name it, against 157 for `LightmappedGeneric`.
    ///
    /// `stdshaders/worldvertextransition.cpp` is 222 lines, of which ~190 are a
    /// parameter table and the remaining three forward to
    /// `InitParamsLightmappedGeneric_DX9`, `InitLightmappedGeneric_DX9` and
    /// `DrawLightmappedGeneric_DX9` — the same helper, the same `.fxc`, the
    /// same vertex format. `lightmappedgeneric_dx9.cpp` even declares
    /// `$basetexture2`, `$bumpmap2`, `$blendmodulatetexture` and `$ssbump`
    /// itself, so the two differ only in which parameters they expose and, in
    /// practice, in whether content happens to set `$basetexture2`.
    ///
    /// It is a separate variant rather than an alias because Valve kept it as a
    /// separate `IShader` —
    /// `DEFINE_FALLBACK_SHADER( WorldVertexTransition, WorldVertexTransition_DX9 )`
    /// — and because [`name`](ShaderKind::name) should say what the `.vmt` said.
    /// Everything else about it delegates: same
    /// [`wgsl`](ShaderKind::wgsl), same [`vertex_layout`](ShaderKind::vertex_layout),
    /// same [`context_binding`](ShaderKind::context_binding), same uniforms.
    WorldVertexTransition,

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
    /// `$basemapalphaphongmask`. That is **317 of the 1,135** materials that
    /// name this shader, and they are [`ShaderKind::Phong`]'s. The redirect is
    /// [`resolve`](ShaderKind::resolve) and the predicate is [`wants_phong`],
    /// so anything holding a `.vmt` and asking "which shader is this" must
    /// call the first of those rather than
    /// [`from_name`](ShaderKind::from_name).
    VertexLitGeneric,

    /// Glass, and anything else that warps what is behind it: the screen-space
    /// refraction shader. 37 of Portal 2's materials name it, 29 of them under
    /// `materials/models/`.
    ///
    /// Models with a specular highlight: the shader a fifth of Portal 2's
    /// models really draw with. **317 of the 1,135 materials that name
    /// `VertexLitGeneric` reach this instead**, 301 of them under
    /// `materials/models/`, and 104 of the game's 106 maps place a static prop
    /// wearing one.
    ///
    /// `stdshaders/phong_dx9_helper.cpp`, `phong_vs20.fxc` and
    /// `phong_ps20b.fxc`.
    ///
    /// **No `.vmt` names it, and that is the one structural thing to know.**
    /// There is no `SHADER( Phong )` anywhere in `stdshaders/` and no
    /// `DEFINE_FALLBACK_SHADER` for it: the helper is reached only from
    /// `DrawVertexLitGeneric_DX9`, which consults `WantsPhongShader` before
    /// doing anything else (`vertexlitgeneric_dx9_helper.cpp:2346`). So
    /// [`from_name`](ShaderKind::from_name) does **not** answer this variant —
    /// [`resolve`](ShaderKind::resolve) does, and that is the function a
    /// caller with a `.vmt` in hand wants. See [`wants_phong`] for the
    /// predicate and [`phong_uniforms`] for the whole combo bucketing.
    ///
    /// It shares [`VertexLitGeneric`](ShaderKind::VertexLitGeneric)'s vertex
    /// layout, its group 3 ([`ContextBinding::ModelLighting`], declared from
    /// the shared `shaders/modellighting.wgsl`) and its parameter table, and
    /// differs from it in what the pixel shader does with them.
    Phong,

    /// `stdshaders/refract.cpp` through `refract_dx9_helper.cpp`,
    /// `Refract_vs20.fxc` and `refract_ps2x.fxc`.
    ///
    /// **It is the first shader in this port that reads the frame it is being
    /// drawn into**, through a copy —
    /// [`ContextBinding::FrameBufferCopy`] — and that is what makes it
    /// structurally different from the four above rather than merely another
    /// set of textures. See
    /// [`needs_frame_buffer_copy`] for which materials actually want the copy
    /// and which supply their own `$basetexture` instead.
    Refract,

    /// The coloured oval a portal wears — `portdocs/PORTAL.md` §7.
    ///
    /// `stdshaders/portal_refract.cpp` through `portal_refract_helper.cpp`,
    /// `portal_refract_vs20.fxc` and `portal_refract_ps2x.fxc`.
    ///
    /// **One shader name, three unrelated pixel shaders, and this variant is
    /// the third.** `$Stage` picks between the see-through warp (0), the
    /// stencil punch (1) and the oval (2), and the first two exist only to
    /// make `CPortalRenderable_FlatBasic`'s recursive view composite — which
    /// is out of scope. So [`resolve`](ShaderKind::resolve) answers this
    /// variant **only for `$Stage 2`** and `None` for the other two, which is
    /// the second thing in the port that `from_name` and `resolve` disagree
    /// about (the first is [`Phong`](ShaderKind::Phong), and it disagrees the
    /// other way).
    ///
    /// Measured over the mounted game: **7 materials name `PortalRefract`, 5
    /// of them stage 2** — the three `models/portals/portalstaticoverlay_*`
    /// and the two `effects/fakeportalring_*`, which reach stage 2 through
    /// `$UseOnStaticProp` rather than through `$Stage`. The other two are
    /// `portal_refract_1` (stage 0) and `portal_stencil_hole` (stage 1), and
    /// nothing in this port draws either.
    ///
    /// It is the first shader here whose group 3 is neither lighting nor a
    /// copy of the frame: three numbers that differ between two portals
    /// wearing the same material, which in the shipped game are material vars
    /// rewritten by proxies. See
    /// [`PortalOverlay`](super::uniforms::PortalOverlay).
    PortalRefract,
}

/// What a shader binds in group 3, if anything.
///
/// Group 3 is **whichever piece of render-context state this shader reads** —
/// state that belongs to neither the material nor the draw call, and that
/// Valve likewise set on `IMatRenderContext` rather than on either
/// (`BindLightmapPage`, `PI_SetVertexShaderAmbientLightCube`,
/// `SetFrameBufferCopyTexture`). A pipeline layout is per shader, so a shader
/// that reads none of it declares no group 3 at all and its draws bind nothing
/// there.
///
/// The first two shapes were both *lighting* — a page of the baked lightmap
/// atlas for brushes, an ambient cube plus local lights for models — which is
/// why this was called `LightingBinding` until `Refract` arrived wanting a
/// copy of the frame buffer in the same slot for the same reason.
///
/// Groups 0, 1 and 2 are frequency groups shared by every shader
/// ([`uniforms`](super::uniforms)); this one is the exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextBinding {
    /// A lightmap atlas page: a texture and its sampler.
    /// [`Pass::bind_lightmap_page`](super::context::Pass::bind_lightmap_page).
    LightmapPage,
    /// [`ModelLighting`](super::uniforms::ModelLighting), bound with a dynamic
    /// offset. [`Pass::set_model_lighting`](super::context::Pass::set_model_lighting).
    ModelLighting,
    /// A readable copy of the scene drawn so far: a texture and its sampler.
    /// `TEXTURE_FRAME_BUFFER_FULL_TEXTURE_0`, pointed at by
    /// `IMatRenderContext::SetFrameBufferCopyTexture`
    /// (`cmatrendercontext.cpp:456`) and filled by
    /// [`RenderContext::update_refract_texture`](super::context::RenderContext::update_refract_texture).
    FrameBufferCopy,
    /// [`PortalOverlay`](super::uniforms::PortalOverlay), bound with a dynamic
    /// offset the way [`ModelLighting`](ContextBinding::ModelLighting) is.
    /// [`Pass::set_portal_overlay`](super::context::Pass::set_portal_overlay).
    ///
    /// **The one shape here that is not render-context state in the original**,
    /// and it is here because there is nowhere else: it is per-*instance*
    /// material data, which Valve carries in the material's own var table and
    /// rewrites with a proxy before each draw. Group 1 is baked at load in this
    /// port, so two portals wearing one material could not differ. Group 3 is
    /// already the per-shader, per-instance slot — that is what
    /// `ModelLighting` is — so it goes there.
    PortalOverlay,
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
    ///
    /// **[`ShaderKind::Phong`] is deliberately not answerable here**, because
    /// no `.vmt` names it: it is not an `IShader` in the original either. Use
    /// [`resolve`](ShaderKind::resolve) when the whole `.vmt` is in hand.
    pub fn from_name(name: &str) -> Option<ShaderKind> {
        match name {
            n if n.eq_ignore_ascii_case("UnlitGeneric") => Some(ShaderKind::UnlitGeneric),
            n if n.eq_ignore_ascii_case("LightmappedGeneric") => {
                Some(ShaderKind::LightmappedGeneric)
            }
            n if n.eq_ignore_ascii_case("WorldVertexTransition") => {
                Some(ShaderKind::WorldVertexTransition)
            }
            n if n.eq_ignore_ascii_case("VertexLitGeneric") => Some(ShaderKind::VertexLitGeneric),
            n if n.eq_ignore_ascii_case("Refract") => Some(ShaderKind::Refract),
            n if n.eq_ignore_ascii_case("PortalRefract") => Some(ShaderKind::PortalRefract),
            _ => None,
        }
    }

    /// Which shader actually draws a `.vmt` — the name it wrote, then the one
    /// redirect the original makes on top of it.
    ///
    /// `CShaderSystem` resolved a material to an `IShader` by name and stopped
    /// there; `DrawVertexLitGeneric_DX9` then hands the material to
    /// `DrawPhong_DX9` when `WantsPhongShader` says so
    /// (`vertexlitgeneric_dx9_helper.cpp:2346`), which is a second dispatch one
    /// layer down. Here both layers are this function, because a `ShaderKind`
    /// is what picks the WGSL module, the bind group layout and the parameter
    /// table, and a Phong material needs all three of Phong's.
    ///
    /// **Call this rather than [`from_name`](ShaderKind::from_name) wherever a
    /// `.vmt` is available.** `from_name` answers "what did the content say",
    /// which is what a diagnostic wants; this answers "what will draw it",
    /// which is what everything else wants.
    pub fn resolve(vmt: &Vmt) -> Option<ShaderKind> {
        let kind = ShaderKind::from_name(&vmt.shader)?;
        if kind == ShaderKind::VertexLitGeneric && wants_phong(vmt) {
            return Some(ShaderKind::Phong);
        }
        // The *other* direction: a name this port knows, on a material it
        // cannot draw. `PortalRefract`'s three stages are three unrelated
        // pixel shaders and only the third is ported, so a stage-0 or stage-1
        // material resolves to nothing and gets the error material — which is
        // the honest answer and is what keeps the depot census truthful. See
        // [`portal_refract_stage`].
        if kind == ShaderKind::PortalRefract && portal_refract_stage(vmt) != 2 {
            return None;
        }
        Some(kind)
    }

    /// The shader's name, in the spelling the original gives it.
    ///
    /// For every variant but one this is also the name content writes.
    /// [`Phong`](ShaderKind::Phong) is the exception — no `.vmt` can name it
    /// — and it still answers `"Phong"`, because what this feeds is
    /// diagnostics, pipeline labels and the depot census, all of which want to
    /// say which shader is running rather than which one was asked for.
    pub fn name(self) -> &'static str {
        match self {
            ShaderKind::UnlitGeneric => "UnlitGeneric",
            ShaderKind::LightmappedGeneric => "LightmappedGeneric",
            ShaderKind::WorldVertexTransition => "WorldVertexTransition",
            ShaderKind::VertexLitGeneric => "VertexLitGeneric",
            ShaderKind::Phong => "Phong",
            ShaderKind::Refract => "Refract",
            ShaderKind::PortalRefract => "PortalRefract",
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
            ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition => {
                VertexLayout::World
            }
            // `VertexShaderVertexFormat( VERTEX_POSITION | VERTEX_NORMAL |
            // VERTEX_COLOR_STREAM_1, 1, {2}, userDataSize )`
            // (`vertexlitgeneric_dx9_helper.cpp:895`).
            //
            // `Phong` asks for the same thing and has no second form at all:
            // *"We always specify we're using user data, therefore we always
            // need tangent spaces"* (`phong_dx9_helper.cpp:95`), so
            // `userDataSize` is an unconditional 4 where `VertexLitGeneric`
            // makes it conditional on `$bumpmap`. This port answers one layout
            // for both, which for `Phong` is not even a simplification.
            ShaderKind::VertexLitGeneric | ShaderKind::Phong => VertexLayout::Model,
            // `Refract` genuinely declares two formats and the axis is
            // `$model`: `VERTEX_POSITION | VERTEX_NORMAL` plus either
            // `userDataSize = 4` (a model, the tangent in user data) or
            // `VERTEX_TANGENT_S | VERTEX_TANGENT_T` (a brush surface)
            // (`refract_dx9_helper.cpp:196`). **Pinned to the model form on a
            // measurement**: 29 of the game's 37 `Refract` materials set
            // `$model 1`, and all eight that do not are under
            // `materials/particle/` — drawn by the particle system, which is
            // not ported. **No brush face and no displacement in the shipped
            // game names this shader**, so the world form has no content to
            // draw. The day particles land, this is where the second layout
            // arrives, and it will want a tangent on a `SimpleVertex` rather
            // than the world one.
            ShaderKind::Refract => VertexLayout::Model,
            // `PortalRefract` declares two formats as well, and unlike
            // `Refract`'s the axis is not content: `$UseOnStaticProp` picks
            // between `VERTEX_POSITION | VERTEX_NORMAL` with two texture
            // coordinates and four floats of user data, and
            // `VERTEX_POSITION | VERTEX_FORMAT_COMPRESSED` with one texture
            // coordinate and none (`portal_refract_helper.cpp:88`).
            //
            // **One layout serves both, and it is the smaller one**, because
            // the stage-2 pixel shader reads neither the normal, the tangent
            // nor the second texture coordinate — the normal and tangent are
            // stage 0's screen-space warp and the second coordinate is a
            // constant `(0.25, 0)` that `DrawSimplePortalMesh` writes and
            // nothing samples. That is a *reduction* rather than a
            // simplification: adding them would mean binding a second
            // vertex stream ([`VertexLayout::Model`] takes the baked static
            // light in slot 1) for a shader with no lighting at all.
            ShaderKind::PortalRefract => VertexLayout::Simple,
        }
    }

    /// Every parameter this shader declares, standard ones first.
    ///
    /// `IShader::GetParamCount`/`GetParamInfo`, which `BEGIN_SHADER_PARAMS`
    /// builds by concatenating `CBaseShader`'s table with the shader's own
    /// (`public/shaderlib/cshader.h:212`).
    pub fn params(self) -> impl Iterator<Item = &'static ShaderParam> {
        // Three slices rather than two, because `WorldVertexTransition`'s table
        // really is `LightmappedGeneric`'s plus two — see
        // [`WORLD_VERTEX_TRANSITION_PARAMS`]. Listing them separately keeps the
        // rule that a table entry is a promise: setting `$basetexturetransform2`
        // on a `LightmappedGeneric` material does nothing in Valve's engine,
        // because that shader does not declare it, and so it does nothing here.
        let (own, extra): (_, &'static [ShaderParam]) = match self {
            ShaderKind::UnlitGeneric => (UNLIT_GENERIC_PARAMS, &[]),
            ShaderKind::LightmappedGeneric => (LIGHTMAPPED_GENERIC_PARAMS, &[]),
            ShaderKind::WorldVertexTransition => {
                (LIGHTMAPPED_GENERIC_PARAMS, WORLD_VERTEX_TRANSITION_PARAMS)
            }
            ShaderKind::VertexLitGeneric => (VERTEX_LIT_GENERIC_PARAMS, &[]),
            // The same declaration plus `PHONG_PARAMS` — see that table for
            // why the split exists where Valve's `BEGIN_SHADER_PARAMS` block
            // has none.
            ShaderKind::Phong => (VERTEX_LIT_GENERIC_PARAMS, PHONG_PARAMS),
            ShaderKind::Refract => (REFRACT_PARAMS, &[]),
            ShaderKind::PortalRefract => (PORTAL_REFRACT_PARAMS, &[]),
        };
        STANDARD_PARAMS.iter().chain(own).chain(extra)
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
    /// so both have to be answerable per shader. See [`ContextBinding`].
    pub fn context_binding(self) -> Option<ContextBinding> {
        match self {
            ShaderKind::UnlitGeneric => None,
            ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition => {
                Some(ContextBinding::LightmapPage)
            }
            ShaderKind::VertexLitGeneric | ShaderKind::Phong => Some(ContextBinding::ModelLighting),
            // Not lighting: `Refract` has none. What it reads out of the
            // render context is the frame it is being drawn into.
            ShaderKind::Refract => Some(ContextBinding::FrameBufferCopy),
            // Nor lighting: a portal's oval is emissive and is lit by nothing.
            // What it reads is its own open amount, which is per instance.
            ShaderKind::PortalRefract => Some(ContextBinding::PortalOverlay),
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
            // One module for two shader names, which is what the original
            // does too: `WorldVertexTransition`'s `SHADER_DRAW` is a call to
            // `DrawLightmappedGeneric_DX9`.
            ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition => {
                include_str!("shaders/lightmappedgeneric.wgsl")
            }
            ShaderKind::VertexLitGeneric => include_str!("shaders/vertexlitgeneric.wgsl"),
            ShaderKind::Phong => include_str!("shaders/phong.wgsl"),
            ShaderKind::Refract => include_str!("shaders/refract.wgsl"),
            ShaderKind::PortalRefract => include_str!("shaders/portalrefract.wgsl"),
        };
        // A second shared fragment, narrower than the prelude: group 3's
        // *layout* is per shader, so a `@group(3)` declaration cannot live in
        // something every shader includes — but it can be shared by the
        // shaders that agree, and `VertexLitGeneric` and `Phong` do. See
        // `shaders/modellighting.wgsl`.
        let lighting = match self.context_binding() {
            Some(ContextBinding::ModelLighting) => include_str!("shaders/modellighting.wgsl"),
            _ => "",
        };
        format!(
            "{}\n{}\n{}",
            include_str!("shaders/prelude.wgsl"),
            lighting,
            body
        )
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
    // The two-layer blend. Declared *here*, on `LightmappedGeneric`, because
    // that is where `lightmappedgeneric_dx9.cpp:53-60` declares them — a world
    // surface can blend two textures without the material naming
    // `WorldVertexTransition`, and 157 of Portal 2's displacement faces do
    // exactly that.
    ShaderParam {
        name: "$basetexture2",
        kind: ParamKind::Texture,
        declared_default: "shadertest/lightmappedtexture",
        help: "the second layer, blended over the first by the vertex alpha",
    },
    ShaderParam {
        name: "$frame2",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "frame number for $basetexture2",
    },
    ShaderParam {
        name: "$bumpmap2",
        kind: ParamKind::Texture,
        declared_default: "models/shadertest/shader3_normal",
        help: "the second layer's bump map",
    },
    ShaderParam {
        name: "$bumpframe2",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "frame number for $bumpmap2",
    },
    ShaderParam {
        name: "$blendmodulatetexture",
        kind: ParamKind::Texture,
        declared_default: "",
        help: "texture to use r/g channels for blend range for",
    },
    ShaderParam {
        name: "$ssbump",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "whether or not to use alternate bumpmap format with height",
    },
];

/// `WorldVertexTransition`'s parameters *beyond*
/// [`LIGHTMAPPED_GENERIC_PARAMS`] (`stdshaders/worldvertextransition.cpp:84`
/// and `:89`).
///
/// Two, and only two, of the ~60 that declaration adds are read here: the
/// transforms for the second base texture and for the blend modulation, which
/// `LightmappedGeneric` genuinely does not declare. Everything else it adds —
/// layer tints, `$newlayerblending` and the border/edge terms, drop shadows,
/// `$detail2`, phong — belongs to a feature listed as deferred in this module's
/// header, and no Portal 2 displacement material sets any of them.
const WORLD_VERTEX_TRANSITION_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$basetexturetransform2",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "$basetexture2 texcoord transform",
    },
    ShaderParam {
        name: "$blendmodulatetransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "$blendmodulatetexture texcoord transform",
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
/// in, which reaches `phong_dx9_helper.cpp` instead — those are
/// [`PHONG_PARAMS`], declared for [`ShaderKind::Phong`] and not for this one,
/// because on a non-phong material they do nothing. **Features not ported**:
/// wrinkle maps, tree sway, decal textures, tint masks, displacement, distance
/// alpha, seamless mapping, self-illum fresnel, the flashlight and cascaded
/// shadow maps.
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

/// `Phong`'s parameters *beyond* [`VERTEX_LIT_GENERIC_PARAMS`].
///
/// **Valve's declaration does not have this split, and the split is
/// deliberate.** `vertexlitgeneric_dx9.cpp`'s `BEGIN_SHADER_PARAMS` block
/// declares `$phongboost` and everything below whether or not a material ends
/// up at `phong_dx9_helper.cpp`, because there is only one `IShader` and it
/// owns the whole table. This module's rule is stricter: a table entry is a
/// promise that setting the parameter does something, and on a
/// `VertexLitGeneric` material that does *not* reach Phong, none of these does
/// anything at all. So they are chained only for [`ShaderKind::Phong`],
/// exactly as [`WORLD_VERTEX_TRANSITION_PARAMS`] is chained only for that
/// name.
///
/// The three *dispatch* parameters stay in the shared table, because
/// [`wants_phong`] reads them through `ShaderKind::VertexLitGeneric` before
/// there is a `Phong` to read them through: `$phong`,
/// `$basemapalphaphongmask` and `$lightwarptexture`.
///
/// Not declared, because the shader does not read them and nothing should
/// imply otherwise: `$forcephong` (`mat_phong` has no video-options page here,
/// so the outer test in `WantsPhongShader` is always true and forcing is
/// redundant), `$ambientocclusion` (Source Filmmaker only), `$rimmask`'s
/// companions `$compress`/`$stretch`/`$bumpcompress`/`$bumpstretch` (wrinkle
/// maps, no Portal 2 content), `$decaltexture`/`$decalblendmode` and
/// `$tintmasktexture` (likewise none), `$selfillumfresnel` and its min/max/exp
/// (a commented-out body and a CS:GO team-ID glow), and `$displacementmap`.
const PHONG_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$phongexponent",
        kind: ParamKind::Float,
        declared_default: "5.0",
        help: "Phong exponent for local specular lights",
    },
    ShaderParam {
        name: "$phongexponenttexture",
        kind: ParamKind::Texture,
        declared_default: "shadertest/BaseTexture",
        help: "Phong Exponent map",
    },
    ShaderParam {
        name: "$phongtint",
        kind: ParamKind::Vec3,
        declared_default: "5.0",
        help: "Phong tint for local specular lights",
    },
    ShaderParam {
        name: "$phongalbedotint",
        kind: ParamKind::Bool,
        declared_default: "1.0",
        help: "Apply tint by albedo (controlled by spec exponent texture",
    },
    ShaderParam {
        name: "$phongalbedoboost",
        kind: ParamKind::Float,
        declared_default: "1.0",
        help: "Phong albedo overbrightening factor (specular mask channel \
               should be authored to account for this)",
    },
    ShaderParam {
        name: "$phongboost",
        kind: ParamKind::Float,
        declared_default: "1.0",
        help: "Phong overbrightening factor (specular mask channel should be \
               authored to account for this)",
    },
    ShaderParam {
        name: "$phongfresnelranges",
        kind: ParamKind::Vec3,
        declared_default: "[0  0.5  1]",
        help: "Parameters for remapping fresnel output",
    },
    ShaderParam {
        name: "$phongwarptexture",
        kind: ParamKind::Texture,
        declared_default: "shadertest/BaseTexture",
        help: "warp the specular term",
    },
    ShaderParam {
        name: "$phongdisablehalflambert",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "Disable half lambert for phong",
    },
    ShaderParam {
        name: "$basemapluminancephongmask",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "indicates that the base luminance should be used to mask phong",
    },
    ShaderParam {
        name: "$invertphongmask",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "invert the phong mask (0=full phong, 1=no phong)",
    },
    ShaderParam {
        name: "$rimlight",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "enables rim lighting",
    },
    ShaderParam {
        name: "$rimlightexponent",
        kind: ParamKind::Float,
        declared_default: "4.0",
        help: "Exponent for rim lights",
    },
    ShaderParam {
        name: "$rimlightboost",
        kind: ParamKind::Float,
        declared_default: "1.0",
        help: "Boost for rim lights",
    },
    ShaderParam {
        name: "$rimmask",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "Indicates whether or not to use alpha channel of exponent \
               texture to mask the rim term",
    },
];

/// `Refract`'s own parameters (`stdshaders/refract.cpp:20`), restricted to the
/// ones this port reads.
///
/// The declaration there has 31. Thirteen are left out rather than
/// declared-and-ignored, because a table entry is a promise that setting the
/// parameter does something — and each of the thirteen is either dead in the
/// original or has no content in Portal 2 to verify against:
///
/// - **`$time` and `$fresnelreflection` are dead in Valve's own shader.**
///   `$time` reaches `g_c5.w` and `SHADER_SPECIFIC_CONST_5`, and neither
///   `refract_ps2x.fxc` nor `Refract_vs20.fxc` reads the register it lands in.
///   `$fresnelreflection` is not written to any register at all — the file
///   carries *"FIXME: doesn't support Fresnel!"* twice, once in `refract.cpp`
///   and once in the helper, and the note is accurate.
/// - **`$normalmap2`, `$bumpframe2` and `$bumptransform2`** drive the
///   `SECONDARY_NORMAL` combo, which **no material in Portal 2 turns on** — and
///   which is broken where it is implemented: the shader binds `$normalmap2` to
///   sampler 1 and then samples *sampler 3*, the first normal map, with the
///   second set of coordinates (`refract_ps2x.fxc:143`).
/// - **`$masked`** needs a blend mode (`ONE_MINUS_SRC_ALPHA`, `SRC_ALPHA`) that
///   [`BlendMode`] does not have, for **zero** materials.
/// - **`$magnifyenable`, `$magnifycenter`, `$magnifyscale`** — zero materials.
/// - **`$noviewportfixup` and `$mirroraboutviewportedges`** are split-screen
///   console code: the viewport fixup is inside `#if defined( _X360 ) ||
///   defined( _PS3 )` in the vertex shader and `bMirrorAboutViewportEdges` is
///   `IsX360() && ...` in the helper, so both are false on this port's only
///   platform.
/// - **`$refracttintextureframe` and `$envmapframe`** are animated-texture
///   frame indices, which the texture path does not select between yet — the
///   same gap `$frame` and `$bumpframe` sit in.
///
/// `$color` and `$alpha` are `SHADER_PARAM_OVERRIDE`n here to say *"unused"*,
/// and they are: the shader never reads `cModulationColor`. `$alpha` is still
/// read by the shadow phase, because `IsAlphaModulating` is what it always was.
const REFRACT_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$refractamount",
        kind: ParamKind::Float,
        declared_default: "2",
        help: "",
    },
    ShaderParam {
        name: "$refracttint",
        kind: ParamKind::Color,
        declared_default: "[1 1 1]",
        help: "refraction tint",
    },
    ShaderParam {
        name: "$normalmap",
        kind: ParamKind::Texture,
        declared_default: "models/shadertest/shader1_normal",
        help: "normal map",
    },
    ShaderParam {
        name: "$bumpframe",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "frame number for $normalmap",
    },
    ShaderParam {
        name: "$bumptransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "$normalmap texcoord transform",
    },
    ShaderParam {
        name: "$bluramount",
        kind: ParamKind::Integer,
        declared_default: "1",
        help: "0, 1, or 2 for how much blur you want",
    },
    ShaderParam {
        name: "$fadeoutonsilhouette",
        kind: ParamKind::Bool,
        declared_default: "1",
        help: "0 for no fade out on silhouette, 1 for fade out on sillhouette",
    },
    ShaderParam {
        name: "$envmap",
        kind: ParamKind::Texture,
        declared_default: "shadertest/shadertest_env",
        help: "envmap",
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
        name: "$refracttinttexture",
        kind: ParamKind::Texture,
        declared_default: "models/shadertest/shield",
        help: "",
    },
    ShaderParam {
        name: "$nowritez",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "0 == write z, 1 = no write z",
    },
    ShaderParam {
        name: "$localrefract",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "",
    },
    ShaderParam {
        name: "$localrefractdepth",
        kind: ParamKind::Float,
        declared_default: "0",
        help: "",
    },
];

/// `PortalRefract`'s own parameters (`stdshaders/portal_refract.cpp:11`), all
/// eleven of them.
///
/// Nothing is left out, which makes this the only shader table in the module
/// that is a straight transliteration — the declaration is small, and every
/// entry either reaches the shader or decides which stage it is.
///
/// Four of them are worth a note.
///
/// - **`$Stage` is not a parameter the shader reads**, it is which of three
///   shaders runs. See [`portal_refract_stage`] and
///   [`ShaderKind::resolve`](ShaderKind::resolve).
/// - **`$UseOnStaticProp` forces the stage to 2** whatever `$Stage` says
///   (`portal_refract_helper.cpp:79`), and changes the vertex format, which is
///   why the two `effects/fakeportalring_*` materials are stage-2 materials
///   without saying so.
/// - **`$PortalStatic` is inverted on the way to the shader.** The register is
///   `g_flPortalActive = 1 - $PortalStatic` (`:216`), so the parameter means
///   "how much interference" and the shader reads "how settled".
/// - **`$PortalOpenAmount`, `$PortalStatic` and `$time` are per *instance*
///   here**, not per material: content drives all three from material proxies
///   and this port has none, so they come from group 3. See
///   [`PortalOverlay`](super::uniforms::PortalOverlay).
///
/// `$color`, `$color2` and `$alpha` are declared by `CBaseShader` and read by
/// nothing: neither `.fxc` declares `cModulationColor` at all. `$alpha` still
/// reaches [`render_state`] through `IsAlphaModulating`, and there it changes
/// nothing, because this shader blends unconditionally.
const PORTAL_REFRACT_PARAMS: &[ShaderParam] = &[
    ShaderParam {
        name: "$stage",
        kind: ParamKind::Integer,
        declared_default: "0",
        help: "Stage of portal rendering (0, 1, 2)",
    },
    ShaderParam {
        name: "$portalopenamount",
        kind: ParamKind::Float,
        declared_default: "0.0",
        help: "Portal open amount 0.0-1.0",
    },
    ShaderParam {
        name: "$portalstatic",
        kind: ParamKind::Float,
        declared_default: "0.0",
        help: "Portal static amount 0.0-1.0",
    },
    ShaderParam {
        name: "$portalmasktexture",
        kind: ParamKind::Texture,
        declared_default: "",
        help: "Mask texture",
    },
    ShaderParam {
        name: "$texturetransform",
        kind: ParamKind::Matrix,
        declared_default: "center .5 .5 scale 1 1 rotate 0 translate 0 0",
        help: "Texcoord transform",
    },
    ShaderParam {
        name: "$portalcolortexture",
        kind: ParamKind::Texture,
        declared_default: "",
        help: "Color texture",
    },
    ShaderParam {
        name: "$portalcolorgradientdark",
        kind: ParamKind::Color,
        declared_default: "[0.0 0.0 0.0]",
        help: "The dark end of a tint gradient if not using a color texture",
    },
    ShaderParam {
        name: "$portalcolorgradientlight",
        kind: ParamKind::Color,
        declared_default: "[1.0 1.0 1.0]",
        help: "The light end of a tint gradient if not using a color texture",
    },
    ShaderParam {
        name: "$portalcolorscale",
        kind: ParamKind::Float,
        declared_default: "0.0",
        help: "Portal color scale",
    },
    ShaderParam {
        name: "$time",
        kind: ParamKind::Float,
        declared_default: "0.0",
        help: "Needs CurrentTime Proxy",
    },
    ShaderParam {
        name: "$useonstaticprop",
        kind: ParamKind::Bool,
        declared_default: "0",
        help: "Activate special mode to use this shader on a static prop",
    },
];

/// Which of `PortalRefract`'s three shaders a `.vmt` asks for.
///
/// `int nStage = IS_PARAM_DEFINED( m_nStage ) ? params[m_nStage]->GetIntValue() : 0;`
/// followed by `if ( bUseOnStaticProp ) { nStage = 2; }`
/// (`portal_refract_helper.cpp:71,79`) — **in that order**, so
/// `$UseOnStaticProp` overrides an explicit `$Stage` rather than defaulting
/// one. Both `effects/fakeportalring_*` materials write `$Stage 2` as well, so
/// no shipped material distinguishes the two readings; a material that wrote
/// `$Stage 0 $UseOnStaticProp 1` would be stage 2.
///
/// `InitParamsPortalRefract` also *writes* the default back into the parameter
/// when it is undefined (`:26`), which is why an undefined `$Stage` is 0 here
/// rather than "unknown".
fn portal_refract_stage(vmt: &Vmt) -> i32 {
    let kind = ShaderKind::PortalRefract;
    if param_value(kind, vmt, "$useonstaticprop").is_some_and(|var| var.as_bool()) {
        return 2;
    }
    param_value(kind, vmt, "$stage")
        .map(|var| var.as_f32() as i32)
        .unwrap_or(0)
}

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

/// The two-layer blend's three textures, which `LightmappedGeneric` and
/// `WorldVertexTransition` share — see
/// [`ShaderKind::WorldVertexTransition`].
pub const BINDING_BASE2_TEXTURE: u32 = 13;
pub const BINDING_BASE2_SAMPLER: u32 = 14;
pub const BINDING_BUMP2_TEXTURE: u32 = 15;
pub const BINDING_BUMP2_SAMPLER: u32 = 16;
pub const BINDING_BLEND_MODULATE_TEXTURE: u32 = 17;
pub const BINDING_BLEND_MODULATE_SAMPLER: u32 = 18;

/// `Refract`'s `$refracttinttexture` — `RefractTintSampler`, sampler 5
/// (`refract_ps2x.fxc:47`). A tint *per texel* of the surface rather than the
/// single `$refracttint` colour, which is how a pane of glass gets dirt on it.
pub const BINDING_REFRACT_TINT_TEXTURE: u32 = 19;
pub const BINDING_REFRACT_TINT_SAMPLER: u32 = 20;

/// `Phong`'s three extra samplers.
///
/// Valve's numbering for them is 7, 2 and 1 (`phong_ps20b.fxc:194`, `:192`,
/// `:191`) — scattered, because the sampler map at the top of
/// `phong_dx9_helper.cpp` was filled in over years and the low slots went to
/// the flashlight and the normalization cubemap. There is no reason to
/// reproduce the scatter, so they continue this module's flat numbering.
///
/// `$phongexponenttexture` carries three unrelated things in three channels:
/// `r` is the specular exponent, `g` scales the albedo tint, and `a` masks the
/// rim term. An undefined one binds the standard white texture
/// (`phong_dx9_helper.cpp:668`), which gives exponent 1, a full albedo tint
/// and an unmasked rim — so the sampler needs no feature flag and the shader
/// reads it unconditionally, as the original does.
pub const BINDING_PHONG_EXPONENT_TEXTURE: u32 = 21;
pub const BINDING_PHONG_EXPONENT_SAMPLER: u32 = 22;
/// `$lightwarptexture`, `DiffuseWarpSampler`: a 1D ramp indexed by the scalar
/// diffuse term, which is how a character gets subsurface-looking falloff.
/// Bound as a 2D texture and sampled at `v = 0.5`, because the `.vtf` is 2D
/// and `tex1D` read the same single row.
pub const BINDING_LIGHTWARP_TEXTURE: u32 = 23;
pub const BINDING_LIGHTWARP_SAMPLER: u32 = 24;
/// `$phongwarptexture`, `SpecularWarpSampler`: a 2D table indexed by
/// `((N·H)^k, fresnel)`, for iridescence. One Portal 2 material has one.
pub const BINDING_PHONGWARP_TEXTURE: u32 = 25;
pub const BINDING_PHONGWARP_SAMPLER: u32 = 26;

/// `PortalRefract`'s two.
///
/// `$PortalMaskTexture` is the noise the flames are made of —
/// `models/portals/noise-blur-256x256`, a 256x256 DXT1 image, **not sRGB**
/// because it is a mask (`EnableSRGBRead( SHADER_SAMPLER1, false )`,
/// `portal_refract_helper.cpp:128`). Its parameter is named for the
/// *stage-0/1* use it no longer has; in stage 2 it is a noise field.
pub const BINDING_PORTAL_MASK_TEXTURE: u32 = 27;
pub const BINDING_PORTAL_MASK_SAMPLER: u32 = 28;
/// `$PortalColorTexture` — **a 256x1 gradient strip**, sampled with `tex1D` in
/// the original and here at `v = 0.5`, the same way `$lightwarptexture` is.
/// sRGB, and the whole of a portal's colour: `portal-blue-color.vtf` and
/// `portal-orange-color.vtf` are 1,669 bytes each and differ in nothing else.
pub const BINDING_PORTAL_COLOR_TEXTURE: u32 = 29;
pub const BINDING_PORTAL_COLOR_SAMPLER: u32 = 30;

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

/// Where the readable copy of the scene is bound, also in group **3**.
///
/// The same two binding numbers as the lightmap page, and not a clash: group 3
/// has a *different layout per [`ContextBinding`]*, and a pipeline only ever
/// declares one of them. `RefractSampler`, sampler 2 in the original
/// (`refract_ps2x.fxc:38`), fed by
/// `BindStandardTexture( SHADER_SAMPLER2, ..., TEXTURE_FRAME_BUFFER_FULL_TEXTURE_0 )`
/// (`refract_dx9_helper.cpp:287`).
///
/// **Only reached when the material defines no `$basetexture`.** A `Refract`
/// material that names one binds *that* as the image to warp — see
/// [`RefractFlags::BASE_TEXTURE`] — which is the whole reason six of Portal 2's
/// `$localrefract` glass materials need no frame-buffer copy at all.
pub const BINDING_REFRACT_SOURCE_TEXTURE: u32 = 0;
pub const BINDING_REFRACT_SOURCE_SAMPLER: u32 = 1;

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
/// sends **317 of the game's 1,135** `VertexLitGeneric` materials to
/// `DrawPhong_DX9` instead. `mat_phong` defaults to 1 and there is no video
/// options page here, so `WantsPhongShader`'s outer `mat_phong` test is taken
/// as true and `$forcephong` is redundant — one material sets it.
///
/// This is the predicate; [`ShaderKind::resolve`] is the redirect built on it,
/// and is what callers should use.
///
/// Measured, because the three branches are not equally used: **all 110 of the
/// unbumped materials arrive through `$basemapalphaphongmask`**, and not one
/// reaches Phong through `$lightwarptexture` alone — every material with a
/// light warp also has a bump map or the alpha mask. So the middle branch
/// below has no shipped content and is kept because the reference has it.
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

/// `STUDIOHDR_FLAGS_USES_BUMPMAPPING` — whether a model wearing this material
/// is lit per pixel, and therefore whether it reads `vrad`'s per-vertex bake.
///
/// `CStudioRenderContext::ComputeModelFlags` (`studiorendercontext.cpp:274`)
/// asks two questions of every material on a model and ORs the answers over
/// the whole model:
///
/// - a `$bumpmap` that is actually used — "FIXME: I'd rather know that the
///   material is definitely using the bumpmap. It could be in the file without
///   actually being used" — which for a model shader is the same test
///   [`vertex_lit_uniforms`] makes for [`VertexLitFlags::BUMPMAP`];
/// - **`$phong` non-zero, on its own**, which is a wider net than
///   [`wants_phong`] casts: a `$phong 1` material with no bump map, no light
///   warp and no base-alpha mask draws as `VertexLitGeneric` and still counts
///   as bumped here.
///
/// It matters because it is what `CModelRender::DrawModelExStaticProp`
/// (`l_studio.cpp:3046`) calls `bStaticLighting`, and that decides which
/// lighting a static prop gets: a bumped model is lit by the light cache and
/// reads no colour mesh at all, an unbumped one is lit by its colour mesh and
/// gets no ambient cube and no local lights. Getting it backwards adds one to
/// the other.
///
/// The `numLightingComponents > 1` half of Valve's test is not here:
/// `r_staticlight_streams` is `"1"` (`vertexlitgeneric_dx9_helper.cpp:54`), so
/// the term is always false and the flag alone decides.
pub fn uses_bumpmapping(vmt: &Vmt) -> bool {
    let kind = ShaderKind::VertexLitGeneric;
    if param_value(kind, vmt, "$phong").is_some_and(|var| var.as_bool()) {
        return true;
    }
    vmt.var("$bumpmap")
        .and_then(|var| var.as_str())
        .is_some_and(|value| !value.is_empty())
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
        ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition => vec![
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
            // The second layer, bound by both shader names because both
            // declare it. An undefined one binds the standard white texture
            // and is never sampled, because `LightmappedFlags::BASE_TEXTURE2`
            // is what turns the blend on.
            TextureRequest {
                param: "$basetexture2",
                binding: BINDING_BASE2_TEXTURE,
                color_space: ColorSpace::Srgb,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$bumpmap2",
                binding: BINDING_BUMP2_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            // **Not colour**, despite being a texture the artist authored: the
            // shader reads `.r` as a blend *width* and `.g` as a blend
            // *centre* (`lightmappedgeneric_ps2_3_x.h:419`), so an sRGB decode
            // would bend the crossfade rather than the picture.
            TextureRequest {
                param: "$blendmodulatetexture",
                binding: BINDING_BLEND_MODULATE_TEXTURE,
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
        // `InitPhong_DX9` (`phong_dx9_helper.cpp:132`), which is the same
        // shape as the shader above — one `LoadTexture` per feature with its
        // sRGB flag spelled out — and differs from it in three places:
        //
        // - **there is no `$envmapmask`**: this shader has no envmap-mask
        //   sampler at all, so the reflection's mask is base alpha or the
        //   normal map's alpha and nothing else. One Portal 2 material sets
        //   the parameter and gets nothing.
        // - **three textures it alone reads** — the specular exponent map, the
        //   diffuse (light) warp and the specular (phong) warp — all three
        //   loaded with no flag (`:174`, `:180`, `:186`), so all three are
        //   data rather than colour. The exponent map is the clearest case:
        //   its `r` is an exponent in the range 1..150.
        // - `$selfillummask` and `$bumpmap` are unchanged, and `$envmap` takes
        //   the same `GetHDRType()` branch, so it is linear in an HDR game.
        ShaderKind::Phong => vec![
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
                param: "$envmap",
                binding: BINDING_ENVMAP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::Cube,
            },
            TextureRequest {
                param: "$phongexponenttexture",
                binding: BINDING_PHONG_EXPONENT_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$lightwarptexture",
                binding: BINDING_LIGHTWARP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$phongwarptexture",
                binding: BINDING_PHONGWARP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
        ],
        // `InitRefract_DX9` (`refract_dx9_helper.cpp:99`), which is four
        // `Load*` calls with their flags spelled out. Two of them differ from
        // the shader above in a way worth noticing:
        //
        // - **`$envmap` is `TEXTUREFLAGS_SRGB` unconditionally** here, where
        //   `VertexLitGeneric` makes it conditional on `GetHDRType()`. The
        //   shadow phase agrees — `EnableSRGBRead( SHADER_SAMPLER4, true )`
        //   with no branch (`:172`) — so a `Refract` cube map is colour and a
        //   `VertexLitGeneric` one, in an HDR game, is not. Valve's asymmetry,
        //   and reproducing it is free.
        // - **`$refracttinttexture` is colour**, unlike every other mask-shaped
        //   texture in the set: it is multiplied straight into the refracted
        //   image (`2.0 * g_RefractTint * tex2D(...).rgb`), so it wants the
        //   sRGB decode the artist authored it against.
        ShaderKind::Refract => vec![
            // **The image being warped, when the material supplies its own.**
            // `BindTexture( SHADER_SAMPLER2, SRGBREAD, m_nBaseTexture, m_nFrame )`
            // (`refract_dx9_helper.cpp:275`) — the *same sampler* the
            // frame-buffer copy would otherwise occupy.
            TextureRequest {
                param: "$basetexture",
                binding: BINDING_BASE_TEXTURE,
                color_space: ColorSpace::Srgb,
                dimension: TextureDimension::D2,
            },
            // `LoadBumpMap`. This is the whole shader: its `xy` is the
            // screen-space offset and its `a` scales it.
            TextureRequest {
                param: "$normalmap",
                binding: BINDING_BUMP_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$refracttinttexture",
                binding: BINDING_REFRACT_TINT_TEXTURE,
                color_space: ColorSpace::Srgb,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$envmap",
                binding: BINDING_ENVMAP_TEXTURE,
                color_space: ColorSpace::Srgb,
                dimension: TextureDimension::Cube,
            },
        ],
        // `InitPortalRefract` (`portal_refract_helper.cpp:51`) — two textures,
        // and it loads them **only for stage 2**, which is the only stage this
        // port resolves. `LoadTexture( m_nPortalMaskTexture )` with no flags
        // and `LoadTexture( m_nPortalColorTexture, TEXTUREFLAGS_SRGB )`, and
        // the shadow phase agrees with both.
        //
        // A stage-2 material's **`$basetexture` is never sampled** and is
        // therefore not requested: `effects/fakeportalring_blue` writes one
        // (`models\portals\dummy-blue`) and stage 2 binds only samplers 1 and
        // 2. Requesting it would upload an image no pixel reads and would put
        // it in front of [`render_state`]'s translucency test, where this
        // shader's answer is fixed.
        ShaderKind::PortalRefract => vec![
            TextureRequest {
                param: "$PortalMaskTexture",
                binding: BINDING_PORTAL_MASK_TEXTURE,
                color_space: ColorSpace::Linear,
                dimension: TextureDimension::D2,
            },
            TextureRequest {
                param: "$PortalColorTexture",
                binding: BINDING_PORTAL_COLOR_TEXTURE,
                color_space: ColorSpace::Srgb,
                dimension: TextureDimension::D2,
            },
        ],
    }
}

/// Whether a material wants a readable copy of the scene drawn so far.
///
/// `MATERIAL_VAR2_NEEDS_POWER_OF_TWO_FRAME_BUFFER_TEXTURE`, which
/// `InitParamsRefract_DX9` sets — together with `MATERIAL_VAR_TRANSLUCENT` —
/// for every `Refract` material **except** the `$localrefract` ones
/// (`refract_dx9_helper.cpp:88`, over the comment *"Local refract doesn't need
/// a copy of the frame buffer and doesn't require the translucent flag"*).
///
/// It reaches the renderer as `ERENDERFLAGS_NEEDS_POWER_OF_TWO_FB`, which is
/// what makes `CRendering3dView::DrawTranslucentRenderables` call
/// `UpdateRefractTexture()` before drawing a renderable
/// (`game/client/viewrender.cpp:6195`). Here it decides the same thing one
/// step coarser — see
/// [`World::draw_refracting`](crate::engine::world::World::draw_refracting).
///
/// Measured: **6 of Portal 2's 37 `Refract` materials set `$localrefract`**, and
/// all six of those also supply a `$basetexture` for the shader to warp
/// instead. So the answer is never "wants no copy and reads one anyway".
pub fn needs_frame_buffer_copy(kind: ShaderKind, vmt: &Vmt) -> bool {
    match kind {
        ShaderKind::Refract => {
            !param_value(kind, vmt, "$localrefract").is_some_and(|var| var.as_bool())
        }
        // **`PortalRefract` falls through to `false`, and its own answer says
        // so twice.** `NeedsPowerOfTwoFrameBufferTexture` returns `true`
        // unconditionally when asked at load — "For setting model flag at load
        // time" — and the *per-frame* question it is really asking,
        // `bCheckSpecificToThisFrame`, is `params[STAGE] == 0`
        // (`portal_refract.cpp:44`). Stage 0 is the shader that reads the
        // scene and it is not ported; stage 2 reads nothing but its own two
        // textures.
        _ => false,
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
        // `Refract` sets neither lighting flag — it is not lit at all, and its
        // colour comes from the frame behind it plus an environment map.
        ShaderKind::UnlitGeneric
        | ShaderKind::VertexLitGeneric
        | ShaderKind::Phong
        | ShaderKind::Refract
        // `PortalRefract` is not lit either: a portal's oval is emissive, and
        // `$PortalColorScale` of 4 is what makes it brighter than anything
        // around it.
        | ShaderKind::PortalRefract => Lighting::None,
        ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition => {
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

    /// `BASETEXTURE2`, and with it `VERTEXALPHATEXBLENDFACTOR` — the two-layer
    /// blend, with the vertex alpha as the factor. What makes a
    /// `WorldVertexTransition` material a blend rather than an ordinary lit
    /// surface, and the flag every displacement in Portal 2's terrain sets.
    pub const BASE_TEXTURE2: u32 = 1 << 4;

    /// `FANCY_BLENDING == 1` — `$blendmodulatetexture`, which turns the linear
    /// crossfade into a per-texel one. Without it the two layers dissolve into
    /// each other instead of dirt settling into the low parts of the rock.
    pub const BLEND_MODULATE: u32 = 1 << 5;

    /// `BUMPMAP2` — the second layer's normal map, lerped with the first by the
    /// same blend factor.
    pub const BUMP_MAP2: u32 = 1 << 6;

    /// `BUMPMAP == 2` — a **self-shadowed** bump map, which is not a normal map
    /// and must not be decoded as one.
    ///
    /// Two things change (`lightmappedgeneric_ps2_3_x.h:322` and `:649`): the
    /// texel is used raw rather than `2 * t - 1`, and the bumped lighting stops
    /// being `saturate(dot(n, basis))²` weights and becomes a plain weighted
    /// sum scaled by `1/√3`. Portal 2's entire blend-terrain set sets
    /// `$ssbump 1`, and so does a good deal of its ordinary world geometry.
    pub const SSBUMP: u32 = 1 << 7;
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
    /// `$basetexturetransform2`, for the second layer. Declared only by
    /// `WorldVertexTransition`; identity for a `LightmappedGeneric` that blends,
    /// which is what `lightmappedgeneric_vs20.fxc`'s `FASTPATH` branch gives it.
    pub base_texture2_transform: [[f32; 4]; 2],
    /// `$blendmodulatetransform`, likewise.
    pub blend_modulate_transform: [[f32; 4]; 2],
    /// `$alphatestreference`, or the fixed-function default of 0.7.
    pub alpha_test_reference: f32,
    /// [`LightmappedFlags`].
    pub flags: u32,
    pub _padding: [u32; 2],
}

/// Builds the material block for a `.vmt`.
pub fn lightmapped_uniforms(kind: ShaderKind, vmt: &Vmt) -> LightmappedUniforms {
    let transform = |name| {
        param_value(kind, vmt, name)
            .map(|var| var.as_matrix())
            .unwrap_or(super::var::IDENTITY)
    };
    let base = transform("$basetexturetransform");
    let bump = transform("$bumptransform");
    let base2 = transform("$basetexturetransform2");
    let modulate = transform("$blendmodulatetransform");

    let defined = |name: &str| {
        vmt.var(name)
            .and_then(|var| var.as_str())
            .is_some_and(|value| !value.is_empty())
    };

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

    // `hasBaseTexture2 = hasBaseTexture && params[BASETEXTURE2]->IsTexture()`
    // (`lightmappedgeneric_dx9_helper.cpp:442`), which also drives
    // `VERTEXALPHATEXBLENDFACTOR` and therefore where the blend factor comes
    // from. Without a `$basetexture` there is nothing to blend *with*, so the
    // conjunction is Valve's and not a guard.
    let blends = defined("$basetexture") && defined("$basetexture2");
    if blends {
        flags |= LightmappedFlags::BASE_TEXTURE2;
    }
    // `nFancyBlendMode = bHasBlendModulateTexture` (`:458`), which the helper
    // clears when there is no second layer to modulate (`:456`).
    if blends && defined("$blendmodulatetexture") {
        flags |= LightmappedFlags::BLEND_MODULATE;
    }
    if defined("$bumpmap") && defined("$bumpmap2") {
        flags |= LightmappedFlags::BUMP_MAP2;
    }
    // `bumpmap_variant = hasSSBump ? 2 : hasBump` (`:686`), where `hasSSBump`
    // is `hasBump && $ssbump` (`:441`) — an `$ssbump` with no `$bumpmap` is not
    // a variant, it is nothing.
    if defined("$bumpmap") && param_value(kind, vmt, "$ssbump").is_some_and(|var| var.as_bool()) {
        flags |= LightmappedFlags::SSBUMP;
    }

    LightmappedUniforms {
        base_texture_transform: [base[0], base[1]],
        bump_transform: [bump[0], bump[1]],
        base_texture2_transform: [base2[0], base2[1]],
        blend_modulate_transform: [modulate[0], modulate[1]],
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
    /// `$envmaptint` in `rgb`, **gamma-decoded on the CPU** by
    /// [`gamma_to_linear_full_range_param`], and `$envmapcontrast` in `w`.
    ///
    /// The decode belongs here rather than in the shader because that is where
    /// Valve does it — the constant is written already-linear by
    /// `SetEnvMapTintPixelShaderDynamicStateGammaToLinear` and the pixel shader
    /// only multiplies (`vertexlit_and_unlit_generic_ps2x.fxc:697`).
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
    // **`$envmaptint` is decoded and the other two are not**, which is Valve's
    // asymmetry rather than an oversight here: the reflection is multiplied
    // into an already-linear cubemap sample, so the tint has to be linear, and
    // `$selfillumtint`/`$detailtint` are handed to the shader as written. See
    // [`gamma_to_linear_full_range_param`] for why it is that decode and not
    // the table one every other tint in this module uses.
    let envmap_tint =
        gamma_to_linear_full_range_param(init_vec(vmt, "$envmaptint", [1.0, 1.0, 1.0, 0.0]));
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

/// Flags in [`PhongUniforms::flags`]. Bucket 2 of `Phong`'s combo split — see
/// [`phong_uniforms`] for the whole bucketing.
#[allow(dead_code)]
pub struct PhongFlags;

impl PhongFlags {
    /// Alpha testing, fixed-function state in D3D9.
    ///
    /// **Not reconciled against the other users of base alpha, unlike
    /// [`VertexLitFlags::ALPHA_TEST`]**, and that is Valve's:
    /// `InitVertexLitGeneric_DX9` returns into `InitPhong_DX9` at `:361`, well
    /// before the *"Don't alpha test if the alpha channel is used for other
    /// purposes"* clear at `:419`. So a Phong material alpha-tests even when
    /// `$selfillum` has claimed base alpha. One of the game's 317 sets
    /// `$alphatest`.
    pub const ALPHA_TEST: u32 = 1 << 0;
    /// `SHADER_FOGMODE_DISABLED`. Set by `$nofog`.
    pub const NO_FOG: u32 = 1 << 1;
    /// The material has a `$bumpmap`.
    ///
    /// **Not a lighting-path switch here.** The original samples the normal
    /// map unconditionally and binds `TEXTURE_NORMALMAP_FLAT` when there is
    /// none (`phong_dx9_helper.cpp:678`), so this only chooses between a
    /// texture fetch and the constant `(0, 0, 1)` that a flat normal map
    /// decodes to. 110 of the 317 are unbumped.
    pub const BUMPMAP: u32 = 1 << 2;
    /// `CUBEMAP`: the material has a usable `$envmap`. 123 set the parameter,
    /// but 92 of those say `env_cubemap`, which names no file — see
    /// [`envmap_name`].
    pub const ENVMAP: u32 = 1 << 3;
    /// `SELFILLUM`: part of the albedo is emitted rather than lit.
    pub const SELFILLUM: u32 = 1 << 4;
    /// A `$selfillummask` texture is bound, so the mask comes from it rather
    /// than from base alpha. Also the `w` of
    /// [`PhongUniforms::shader_controls2`], because the shader needs it as a
    /// number in the alpha arithmetic as well as as a branch.
    pub const SELFILLUM_MASK: u32 = 1 << 5;
    /// `DETAILTEXTURE`: a `$detail` texture is bound.
    pub const DETAIL: u32 = 1 << 6;
    /// `RIMLIGHT`: `$rimlight`, which also needs `$phong` — and `r_rimlight`,
    /// a cheat cvar that defaults on and is not ported.
    pub const RIMLIGHT: u32 = 1 << 7;
    /// `LIGHTWARPTEXTURE`: `$lightwarptexture` replaces the scalar diffuse
    /// term with a 1D ramp lookup, and suppresses the half-Lambert square.
    pub const LIGHTWARP: u32 = 1 << 8;
    /// `PHONGWARPTEXTURE`: `$phongwarptexture` warps the specular term by
    /// `((N·H)^k, fresnel)`, and takes the fresnel multiply away from the
    /// specular result.
    pub const PHONGWARP: u32 = 1 << 9;
    /// `PHONG_HALFLAMBERT`, **on by default** — the reverse of
    /// [`VertexLitFlags::HALF_LAMBERT`], which is off unless `$halflambert`
    /// says otherwise. See [`phong_uniforms`].
    pub const HALF_LAMBERT: u32 = 1 << 10;
}

/// `Phong`'s material block — group 1, binding 0.
///
/// The four-component packings are Valve's own constant registers rather than
/// a convenience here, and two of them are facts worth keeping: the rim
/// exponent really does live in the `w` of the specular tint, and the detail
/// blend factor really does share a slot with `$phongalbedoboost`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct PhongUniforms {
    /// `$basetexturetransform`, two rows dotted against `(u, v, 0, 1)`.
    pub base_texture_transform: [[f32; 4]; 2],
    /// `$bumptransform`, `cBumpTexCoordTransform`.
    pub bump_transform: [[f32; 4]; 2],
    /// `cDetailTexCoordTransform`: `$detailtexturetransform` scaled by
    /// `$detailscale`.
    pub detail_transform: [[f32; 4]; 2],
    /// `g_SelfIllumTint_and_DetailBlendFactorOrPhongAlbedoBoost`.
    /// `$selfillumtint` in `rgb`; `w` is `$detailblendfactor` when the material
    /// has a `$detail` and `$phongalbedoboost` when it does not
    /// (`phong_dx9_helper.cpp:610`). **One register, two meanings** — the
    /// reference's own variable name says so, and the pixel shader has two
    /// spellings of the albedo-tint branch for exactly this reason. Free on
    /// content: no Portal 2 material sets `$phongalbedoboost`.
    pub selfillum_tint: [f32; 4],
    /// `g_vPsConst2`. `$envmaptint` in `rgb` — **in gamma space, undecoded** —
    /// and `w` set when the envmap mask is the normal map's alpha rather than
    /// base alpha.
    ///
    /// The gamma space is the point. `DrawPhong_DX9` builds this constant with
    /// a plain `GetVecValue` (`:800`), where `DrawVertexLitGeneric_DX9` reaches
    /// `SetEnvMapTintPixelShaderDynamicStateGammaToLinear` and `Refract`
    /// reaches the 256-entry table — three shaders, three answers. See
    /// [`gamma_to_linear_full_range_param`], and do not "fix" the asymmetry:
    /// `[0.05 0.05 0.05]` decoded is 0.0014, a factor of 36.
    pub envmap_tint: [f32; 4],
    /// `g_FresnelSpecParams`: `$phongfresnelranges` in `xyz`, `$phongboost` in
    /// `w`.
    pub fresnel_spec: [f32; 4],
    /// `g_SpecularRimParams`: `$phongtint` in `xyz` — or `x` negative, meaning
    /// "tint with the albedo" — and the rim exponent in `w`.
    pub specular_rim: [f32; 4],
    /// `g_ShaderControls`, four unrelated `lerp` weights in one register:
    /// `x` = `$basemapalphaphongmask`, `y` unused, `z` = the inverse of
    /// `$blendtintbybasealpha` (and **-1** under `$notint`), `w` =
    /// `$invertphongmask`.
    pub shader_controls: [f32; 4],
    /// `g_ShaderControls2`: `x` = `$envmapfresnel`, `y` =
    /// `$basemapluminancephongmask`, `z` = `$phongexponent` (**0 meaning "read
    /// the exponent map"**), `w` = 1 when a `$selfillummask` is bound.
    pub shader_controls2: [f32; 4],
    /// `x` = the rim mask control, `y` = `$rimlightboost`.
    ///
    /// Valve splits these across two *repurposed flashlight registers* on the
    /// PC path — `PSREG_FLASHLIGHT_ATTENUATION.x` and
    /// `PSREG_FLASHLIGHT_POSITION_RIM_BOOST.w`, with a comment on each saying
    /// it is overridden — and packs them into `PSREG_RIMPARAMS` on console,
    /// where the single-pass flashlight needs its own registers back. This is
    /// the console packing, because there is no flashlight here to take
    /// registers from.
    pub rim_params: [f32; 4],
    /// `$alphatestreference`, or the fixed-function default of 0.7.
    pub alpha_test_reference: f32,
    /// `$detailblendmode`, one of [`detail_blend`]'s values.
    pub detail_blend_mode: i32,
    /// [`PhongFlags`].
    pub flags: u32,
    pub _padding: u32,
}

/// Builds `Phong`'s material block for a `.vmt`.
///
/// # The combo bucketing
///
/// `phong_ps20b.fxc` declares 19 static and 8 dynamic axes and **not one
/// survives as a pipeline variant**, so this shader is a single pipeline shape
/// and its variety comes from [`render_state`] alone. The full table is in
/// `portdocs/MATERIALSYSTEM.md` §9's stage 6; the short form is that eleven
/// axes are pinned by the platform or by an unported feature (`SFM`, the
/// flashlight and its two shadow axes, `UBERLIGHT`, the four CSM axes,
/// `SHADER_SRGB_READ`, `WORLD_NORMAL`, the two dest-alpha axes), four are
/// pinned **on a content measurement** (`WRINKLEMAP`, `DECAL_BLEND_MODE`,
/// `TINTMASKTEXTURE`, `SELFILLUMFRESNEL`), and the rest are the flags above
/// plus `$detailblendmode` and `NUM_LIGHTS`.
///
/// # Three CS:GO-shaped defaults, and the third is reversed here
///
/// `PORTING.md`'s standing warning about the `cstrike15` base arrives in this
/// shader for the third time. The first two were found in `VertexLitGeneric`
/// and are already reversed there: `bHalfLambert` hard-coded `false`, and
/// `SoftenCosineTerm`. The third is **`bPhongHalfLambert`**
/// (`phong_dx9_helper.cpp:479`):
///
/// ```text
/// //bool bPhongHalfLambert = false; IS_PARAM_DEFINED( info.m_nPhongDisableHalfLambert ) ? (params[...]->GetIntValue() == 0) : true;
/// // Disabling half-lambert for CSGO (not 'compatible' with CSM's - fixes bad shadow aliasing on viewmodels in particular).
/// bool bPhongHalfLambert = false;
/// ```
///
/// — and the parameter's own declaration says what the commented-out line
/// means: *"Half lambert has always been forced on in phong, so the only safe
/// way to allow artists to disable half lambert is to create this param that
/// disables the default behavior of forcing half lambert on."* So Portal 2's
/// Phong is half-Lambert **on**, and `$phongdisablehalflambert 1` is the only
/// way off.
///
/// **The content proves it rather than the comment.** 26 of the 317 materials
/// write the parameter and **20 of them write `1`**, which would be a no-op
/// against an off-by-default. And note it is *not* the `$halflambert` material
/// flag, which this shader never reads — 25 of the 317 set that flag and get
/// nothing from it.
///
/// # Parameters content sets that this shader ignores
///
/// Each is Valve's, and each is worth knowing because the numbers are not
/// small: `$envmapcontrast` (51 materials), `$envmapsaturation` (24),
/// `$envmapmask` (1 — there is no envmap-mask sampler at all),
/// `$basealphaenvmapmask` (18 — base alpha is this shader's envmap mask
/// whether the flag is set or not, so it changes nothing),
/// `$selfillummaskscale`, `$halflambert` (25), `$multiply` (0) and
/// `$detailtint` (0).
pub fn phong_uniforms(vmt: &Vmt) -> PhongUniforms {
    let kind = ShaderKind::Phong;
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
    // `params[X]->GetIntValue() != 0`, false for a parameter nobody set —
    // which is what `InitParamsPhong_DX9`'s `InitIntParam( X, params, 0 )`
    // leaves behind.
    let boolean = |name| value(name).is_some_and(|var| var.as_bool());
    // The shader wants these as `lerp` weights, which is how a shader model
    // with no branches spelled a boolean.
    let one_if = |condition: bool| if condition { 1.0f32 } else { 0.0 };

    let base = transform("$basetexturetransform");
    let bump = transform("$bumptransform");
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

    // `ComputePhongShaderInfo` (`phong_dx9_helper.cpp:269`).
    let has_bump = defined("$bumpmap");
    let has_envmap = envmap_name(vmt).is_some();
    let has_detail = defined("$detail");
    let has_exponent_texture = defined("$phongexponenttexture");
    let has_lightwarp = defined("$lightwarptexture");
    let has_phongwarp = defined("$phongwarptexture");
    // `r_rimlight` is a cheat cvar defaulting to 1 and is not registered here,
    // so it is taken as on. `m_bHasPhong` is true by construction: a material
    // only reaches this shader through `wants_phong`.
    let has_rim = boolean("$rimlight");
    // Same reading as `vertex_lit_uniforms`: `InitPhong_DX9:157` clears
    // `MATERIAL_VAR_SELFILLUM` when the base texture has no alpha to hold the
    // mask, and the texture is not available here, so the flag is taken at face
    // value — fully emissive rather than subtly wrong.
    let has_selfillum = vmt.flags.contains(MaterialFlags::SELFILLUM);
    let has_selfillum_mask = has_selfillum && defined("$selfillummask");

    let mut flags = 0;
    if vmt.flags.contains(MaterialFlags::ALPHATEST) {
        flags |= PhongFlags::ALPHA_TEST;
    }
    if vmt.flags.contains(MaterialFlags::NOFOG) {
        flags |= PhongFlags::NO_FOG;
    }
    if has_bump {
        flags |= PhongFlags::BUMPMAP;
    }
    if has_envmap {
        flags |= PhongFlags::ENVMAP;
    }
    if has_selfillum {
        flags |= PhongFlags::SELFILLUM;
    }
    if has_selfillum_mask {
        flags |= PhongFlags::SELFILLUM_MASK;
    }
    if has_detail {
        flags |= PhongFlags::DETAIL;
    }
    if has_rim {
        flags |= PhongFlags::RIMLIGHT;
    }
    if has_lightwarp {
        flags |= PhongFlags::LIGHTWARP;
    }
    if has_phongwarp {
        flags |= PhongFlags::PHONGWARP;
    }
    // On unless the material opts out — see this function's header.
    if !boolean("$phongdisablehalflambert") {
        flags |= PhongFlags::HALF_LAMBERT;
    }

    // `float vSpecularTint[4] = {1, 1, 1, 4}` (`:820`), whose `w` is the rim
    // exponent's default. `$phongtint` overwrites `xyz` only.
    let mut specular_tint = match value("$phongtint") {
        Some(var) => {
            let tint = var.as_vec4();
            [tint[0], tint[1], tint[2], 4.0]
        }
        None => [1.0, 1.0, 1.0, 4.0],
    };
    if has_rim {
        if let Some(var) = value("$rimlightexponent") {
            // "Make sure this is at least 1". Seven distinct values ship and
            // three of them — 0.2, 0.5 and 0.8, on 19 materials — are below
            // it, so this clamp is load-bearing rather than defensive.
            specular_tint[3] = var.as_f32().max(1.0);
        }
    }
    // "If it's all zeros, there was no constant tint in the vmt" — and what
    // happens next depends on whether there is a map to tint *from*.
    // `bHasPhongTintMap` is `$phongexponenttexture` **and** `$phongalbedotint`,
    // and `-1` in `x` is the flag the pixel shader tests.
    //
    // Measured: four materials write `$phongtint "[0 0 0]"` — the four
    // `paint/bridge_paint_*` — and **none of them has an exponent texture**, so
    // every one takes the white substitution and the shader's albedo-tint
    // branch is unreachable on shipped content. It is ported anyway, because
    // pinning it off would be a divergence to explain rather than a saving.
    if specular_tint[..3].iter().all(|channel| *channel == 0.0) {
        if has_exponent_texture && boolean("$phongalbedotint") {
            specular_tint[0] = -1.0;
        } else {
            specular_tint[0] = 1.0;
            specular_tint[1] = 1.0;
            specular_tint[2] = 1.0;
        }
    }

    // `float vFresnelRanges_SpecBoost[4] = {0, 0.5, 1, 1}` (`:821`).
    let ranges = match value("$phongfresnelranges") {
        Some(var) => var.as_vec4(),
        None => [0.0, 0.5, 1.0, 0.0],
    };

    // One register, two meanings — see [`PhongUniforms::selfillum_tint`].
    let blend_factor_or_albedo_boost = if has_detail {
        float("$detailblendfactor", 1.0)
    } else {
        float("$phongalbedoboost", 1.0)
    };
    let selfillum_tint = init_vec(vmt, "$selfillumtint", [1.0, 1.0, 1.0, 0.0]);
    // **Undecoded**, unlike `VertexLitGeneric`'s. See
    // [`PhongUniforms::envmap_tint`].
    let envmap_tint = init_vec(vmt, "$envmaptint", [1.0, 1.0, 1.0, 0.0]);

    PhongUniforms {
        base_texture_transform: [base[0], base[1]],
        bump_transform: [bump[0], bump[1]],
        detail_transform: detail,
        selfillum_tint: [
            selfillum_tint[0],
            selfillum_tint[1],
            selfillum_tint[2],
            blend_factor_or_albedo_boost,
        ],
        envmap_tint: [
            envmap_tint[0],
            envmap_tint[1],
            envmap_tint[2],
            one_if(vmt.flags.contains(MaterialFlags::NORMALMAPALPHAENVMAPMASK)),
        ],
        fresnel_spec: [ranges[0], ranges[1], ranges[2], float("$phongboost", 1.0)],
        specular_rim: specular_tint,
        shader_controls: [
            one_if(boolean("$basemapalphaphongmask")),
            0.0,
            // `bNoTint ? -1.0f : ( 1.0f - fBlendTintByBaseAlpha )` (`:768`).
            // The saturate in the shader turns -1 into "no modulation at all",
            // 0 into "base alpha decides" and 1 into "all of it".
            if boolean("$notint") {
                -1.0
            } else {
                1.0 - one_if(boolean("$blendtintbybasealpha"))
            },
            one_if(boolean("$invertphongmask")),
        ],
        shader_controls2: [
            // Written only when there is an envmap to apply it to, which is
            // the reference's own guard (`:920`).
            if has_envmap {
                float("$envmapfresnel", 0.0)
            } else {
                0.0
            },
            one_if(boolean("$basemapluminancephongmask")),
            // **Zero is a sentinel, not a value**: it tells the shader to read
            // the exponent out of `$phongexponenttexture`'s red channel
            // instead. 71 of the 317 materials have an exponent texture and no
            // `$phongexponent`, so that is the live path rather than a
            // fallback. `$phongexponent 0` in a `.vmt` would mean the same
            // thing, which is Valve's overloading and not this port's.
            value("$phongexponent")
                .map(|var| var.as_f32())
                .unwrap_or(0.0),
            one_if(has_selfillum_mask),
        ],
        rim_params: [
            // `bHasRimMaskMap ? params[$rimmask]->GetFloatValue() : 0`, and
            // `bHasRimMaskMap` needs the exponent texture as well as the flag,
            // because the mask lives in that texture's alpha. **No Portal 2
            // material sets `$rimmask`**, so this is 0 throughout the shipped
            // game and `fRimMask` is 1.
            if has_exponent_texture && has_rim && boolean("$rimmask") {
                float("$rimmask", 0.0)
            } else {
                0.0
            },
            if has_rim {
                float("$rimlightboost", 1.0)
            } else {
                1.0
            },
            0.0,
            0.0,
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

/// Flags in [`RefractUniforms::flags`]. Bucket 2 of `Refract`'s combo split —
/// see [`refract_uniforms`] for the whole bucketing.
#[allow(dead_code)]
pub struct RefractFlags;

impl RefractFlags {
    /// `SHADER_FOGMODE_DISABLED`. Set by `$nofog`; `DefaultFog()` otherwise.
    pub const NO_FOG: u32 = 1 << 0;
    /// **The material supplies the image to warp itself**, as `$basetexture`,
    /// rather than reading the frame-buffer copy in group 3. Valve's
    /// `if ( params[BASETEXTURE]->IsTexture() )` at
    /// `refract_dx9_helper.cpp:273`, which picks between
    /// `BindTexture( SHADER_SAMPLER2, ..., m_nBaseTexture )` and
    /// `BindStandardTexture( SHADER_SAMPLER2, ..., TEXTURE_FRAME_BUFFER_FULL_TEXTURE_0 )`
    /// — one sampler, two sources. 7 of the game's 37 materials take the first
    /// branch. See [`needs_frame_buffer_copy`].
    pub const BASE_TEXTURE: u32 = 1 << 1;
    /// `CUBEMAP`: the material has a usable `$envmap`.
    pub const ENVMAP: u32 = 1 << 2;
    /// `REFRACTTINTTEXTURE`: `$refracttinttexture` tints the refraction per
    /// texel instead of `$refracttint` tinting it uniformly.
    pub const REFRACT_TINT_TEXTURE: u32 = 1 << 3;
    /// `BLUR`, which is an `0..1` axis rather than a count — see
    /// [`refract_uniforms`] on why `$bluramount 2` is unreachable. Takes the
    /// four-tap polyphase kernel that stands in for a 3x3 box blur.
    pub const BLUR: u32 = 1 << 4;
    /// `FADEOUTONSILHOUETTE`: the warp fades out where the surface turns away
    /// from the eye, leaving the unwarped image at the silhouette.
    pub const FADE_OUT_ON_SILHOUETTE: u32 = 1 << 5;
    /// `LOCALREFRACT`: refract *within* the material's own texture instead of
    /// across the screen. **Replaces the result the blur path computed**
    /// rather than adding to it — see [`refract_uniforms`].
    pub const LOCAL_REFRACT: u32 = 1 << 6;
}

/// `Refract`'s material block — group 1, binding 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct RefractUniforms {
    /// `$bumptransform`, two rows dotted against `(u, v, 0, 1)` —
    /// `cBumpTexCoordTransform[0..1]`, VS `SHADER_SPECIFIC_CONST_1`. It is the
    /// *only* texture transform this shader has: `$basetexture`,
    /// `$refracttinttexture` and the local-refract lookups all use the same
    /// coordinate (`refract_ps2x.fxc:151,283`), because the base texture here
    /// is an image being warped rather than a surface texture.
    pub bump_transform: [[f32; 4]; 2],
    /// `g_RefractTint`, PS `c1`, **gamma-decoded on the CPU** — see
    /// [`gamma_to_linear_param`]. `w` is unused.
    pub refract_tint: [f32; 4],
    /// `g_EnvmapTint` (PS `c0`, also gamma-decoded) in `rgb`, and
    /// `g_EnvmapContrast` (PS `c2`) in `w`.
    pub envmap_tint: [f32; 4],
    /// The four loose scalars, in the order the original's registers hold
    /// them: `g_RefractScale` (`$refractamount`, PS `c5.x`),
    /// `g_EnvmapSaturation` (PS `c3`), `g_vRefractTextureAspectFixup.x`
    /// (PS `c7.x`) and `g_flRefractDepth` (`$localrefractdepth`, PS `c7.z`).
    pub refract_params: [f32; 4],
    /// [`RefractFlags`].
    pub flags: u32,
    pub _padding: [u32; 3],
}

/// Builds the material block for a `Refract` `.vmt`.
///
/// # The combo bucketing
///
/// `portdocs/MATERIALSYSTEM.md` §7.3 asks for every `STATIC`/`DYNAMIC` axis to
/// be sorted into one of three buckets and the result written down. `Refract`
/// declares 2 vertex axes and 13 pixel axes across `Refract_vs20.fxc` and
/// `refract_ps2x.fxc`:
///
/// **Bucket 1 — pinned, axis deleted.** `SKINNING` and `COMPRESSED_VERTS` (no
/// skinning yet, and `ModelVertex` is unpacked); `MODEL`, pinned to 1 —
/// see [`ShaderKind::vertex_layout`]; `SHADER_SRGB_READ`, which is
/// `IsOSX() && ( FakeSRGBWrite() || !CanDoSRGBReadFromRTs() )`, both false for
/// `wgpu`; `MIRRORABOUTVIEWPORTEDGES`, guarded by `IsX360()`;
/// `WRITE_DEPTH_TO_DESTALPHA` and `D_NVIDIA_STEREO`, neither of which has a
/// counterpart here; and four axes pinned **on a content measurement** rather
/// than on a capability — `SECONDARY_NORMAL`, `MASKED`, `MAGNIFY` and
/// `COLORMODULATE`, which **no Portal 2 material turns on**. The last of those
/// is the only one worth a second look: `$vertexcolormodulate` is set by eight
/// materials, all of them under `materials/particle/`, and a particle is not a
/// static prop — the vertex colour a `ModelVertex` carries is `vrad`'s baked
/// light, not a modulation, so reading it here would tint glass by the lighting
/// of whatever mesh it was welded to.
///
/// **Bucket 2 — a uniform branch**, all of it in [`RefractFlags`]: `CUBEMAP`,
/// `REFRACTTINTTEXTURE`, `LOCALREFRACT`, `FADEOUTONSILHOUETTE`, `BLUR`, the
/// fog mode, and the base-texture-versus-frame-buffer choice that was not a
/// combo at all but a `BindTexture`/`BindStandardTexture` fork.
///
/// **Bucket 3 — a real pipeline variant.** Everything in [`RenderState`]; see
/// [`render_state`], which has its own arm for this shader because `Refract`
/// decides blending from the **normal map** rather than from the base texture.
///
/// # `$bluramount` is a 0-or-1 axis, and the reason is an integer cast
///
/// `BLUR` is declared `"0..1"`, the helper clamps to `MAXBLUR` of 1, and the
/// `BLUR > 1` branch in the pixel shader is therefore unreachable. What decides
/// which side a material lands on is that `$bluramount` is declared
/// `SHADER_PARAM_TYPE_INTEGER` and read with `GetIntValue()`, which
/// **truncates**: the 16 Portal 2 materials that write `$bluramount ".3"` and
/// the two that write `".25"` all mean 0. Measured over the game: 11 materials
/// get the blur, 26 do not.
///
/// # `$localrefract` overwrites the blur, it does not blend with it
///
/// `refract_ps2x.fxc`'s `#if ( LOCALREFRACT )` block assigns `vResult.rgb`
/// outright (`:295`), after the `BLUR` block above it has already assigned it.
/// So a material with both — and all six `$localrefract` materials in the game
/// set `$bluramount 1` — computes the four-tap blur and throws it away. That is
/// Valve's, it is what the six shipped glass materials look like, and the
/// branch order here is the same so that they keep looking like it.
pub fn refract_uniforms(vmt: &Vmt, textures: ResolvedTextures) -> RefractUniforms {
    let kind = ShaderKind::Refract;
    let value = |name| param_value(kind, vmt, name);
    let defined = |name| {
        vmt.var(name)
            .and_then(|var| var.as_str())
            .is_some_and(|value| !value.is_empty())
    };

    let bump = value("$bumptransform")
        .map(|var| var.as_matrix())
        .unwrap_or(super::var::IDENTITY);

    let has_envmap = envmap_name(vmt).is_some();

    let mut flags = 0;
    if vmt.flags.contains(MaterialFlags::NOFOG) {
        flags |= RefractFlags::NO_FOG;
    }
    if defined("$basetexture") {
        flags |= RefractFlags::BASE_TEXTURE;
    }
    if has_envmap {
        flags |= RefractFlags::ENVMAP;
    }
    if defined("$refracttinttexture") {
        flags |= RefractFlags::REFRACT_TINT_TEXTURE;
    }
    // `GetIntValue()`, clamped to `MAXBLUR` — see the note above on why this is
    // a flag and not a number.
    if value("$bluramount").is_some_and(|var| var.as_i32() >= 1) {
        flags |= RefractFlags::BLUR;
    }
    if value("$fadeoutonsilhouette").is_some_and(|var| var.as_bool()) {
        flags |= RefractFlags::FADE_OUT_ON_SILHOUETTE;
    }
    if value("$localrefract").is_some_and(|var| var.as_bool()) {
        flags |= RefractFlags::LOCAL_REFRACT;
    }

    // `float c7[4] = { float(nHeight / nWidth), 1.0f, ... }`
    // (`refract_dx9_helper.cpp:283`) — and `nHeight` and `nWidth` are `int`s,
    // so **that division is integer division** and the "aspect fixup" is a
    // whole number. It is not a rounding slip that happens to be harmless:
    // `glass/refract_light_color` is 128x512, so the five glass materials that
    // name it get 4, while `glass/refract_light_color_container` is square and
    // gets 1 — and a *wider*-than-tall source would get **0** and lose the
    // local refraction's horizontal offset entirely. Reproduced, because it is
    // what the shipped glass looks like.
    //
    // Taken from the resolved `$basetexture` rather than from the frame-buffer
    // copy, which is the other thing the original measures here. The two agree
    // wherever the number is read: `g_vRefractTextureAspectFixup` is used only
    // by the `LOCALREFRACT` branch, and all six `$localrefract` materials in
    // the game define a `$basetexture`. A `$localrefract` material without one
    // would want this per *frame*, from the window size.
    let aspect_fixup = textures
        .base
        .map(|facts| (facts.height / facts.width.max(1)) as f32)
        .unwrap_or(1.0);

    // `SetPixelShaderConstantGammaToLinear( 0, ENVMAPTINT )` and `( 1,
    // REFRACTTINT )` (`refract_dx9_helper.cpp:334`). Both are decoded on the
    // CPU, unlike `LightmappedGeneric`'s tints, so the shader receives linear
    // numbers and multiplies them into an already-linear texture sample.
    let envmap_tint = gamma_to_linear_param(init_vec(vmt, "$envmaptint", [1.0, 1.0, 1.0, 0.0]));
    let refract_tint = gamma_to_linear_param(init_vec(vmt, "$refracttint", [1.0, 1.0, 1.0, 0.0]));

    RefractUniforms {
        bump_transform: [bump[0], bump[1]],
        refract_tint,
        envmap_tint: [
            envmap_tint[0],
            envmap_tint[1],
            envmap_tint[2],
            // `InitParamsRefract_DX9` writes 0 for an undefined
            // `$envmapcontrast` and 1 for an undefined `$envmapsaturation`,
            // which is the `SHADER_INIT_PARAMS` mechanism rather than the
            // type's — hence `init_float` and not `param_value`.
            init_float(vmt, "$envmapcontrast", 0.0),
        ],
        refract_params: [
            // `$refractamount` has no `SHADER_INIT_PARAMS` default, so an
            // undefined one really is 0 and the material does not warp — which
            // is why this is `param_value` (the type default, also 0) rather
            // than `init_float`. Every one of the game's 37 materials sets it,
            // one of them only through a conditional key:
            // `hud/camera_viewfinder_ul` writes `!sonyps3?$refractamount` and
            // would read 0 if `Vmt` did not resolve those.
            value("$refractamount")
                .map(|var| var.as_f32())
                .unwrap_or(0.0),
            init_float(vmt, "$envmapsaturation", 1.0),
            aspect_fixup,
            // 0.05 in `InitParamsRefract_DX9`, against the declared default of
            // 0 — the two disagree and the code wins.
            init_float(vmt, "$localrefractdepth", 0.05),
        ],
        flags,
        _padding: [0; 3],
    }
}

/// Flags in [`PortalRefractUniforms::flags`]. Bucket 2 of `PortalRefract`'s
/// combo split — see [`portal_refract_uniforms`].
#[allow(dead_code)]
pub struct PortalRefractFlags;

impl PortalRefractFlags {
    /// `$nofog`, which `CBaseShader::DefaultFog` honours. No shipped
    /// `PortalRefract` material sets it.
    pub const NO_FOG: u32 = 1 << 0;
    /// The `TINTED` static combo: the gradient comes from
    /// `$PortalColorGradientDark`/`Light` rather than from
    /// `$PortalColorTexture`.
    pub const TINTED: u32 = 1 << 1;
}

/// `PortalRefract`'s material block — group 1, binding 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct PortalRefractUniforms {
    /// `$TextureTransform`, VS `SHADER_SPECIFIC_CONST_1`, as two rows.
    ///
    /// **Only `.xy` of each row is read**, so the translation column does
    /// nothing — see the vertex shader. No shipped material sets the
    /// parameter, so all five get the identity.
    pub texture_transform: [[f32; 4]; 2],
    /// `g_vGradientDark`, PS `c7`. `w` is unused.
    pub gradient_dark: [f32; 4],
    /// `g_vGradientLight`, PS `c8`.
    pub gradient_light: [f32; 4],
    /// `x` is `g_flPortalColorScale`, PS `c4.z`. The rest is padding: the two
    /// registers beside it, `g_flPortalOpenAmount` and `g_flPortalActive`, are
    /// per instance and live in [`PortalOverlay`](super::uniforms::PortalOverlay).
    pub params: [f32; 4],
    /// [`PortalRefractFlags`].
    pub flags: u32,
    pub _padding: [u32; 3],
}

/// Builds the material block for a stage-2 `PortalRefract` `.vmt`.
///
/// # The combo bucketing
///
/// `portal_refract_vs20.fxc` declares 3 axes and `portal_refract_ps2x.fxc`
/// declares 4, and after the stage split there is almost nothing left.
///
/// **Bucket 1 — pinned, axis deleted.** `STAGE`, pinned to 2 by
/// [`ShaderKind::resolve`] — the other two values are two different shaders,
/// not two variants of one, and neither is ported. `USEONSTATICPROP`, which
/// only picks a vertex format, and whose two formats this port answers with
/// one (see [`ShaderKind::vertex_layout`]). `COMPRESSED_VERTS`, which the
/// port's vertex structs are not. `SHADER_SRGB_READ`, false off the 360.
/// `D_NVIDIA_STEREO`, which has no counterpart here — and note the sampler it
/// binds, `SHADER_SAMPLER3`, is enabled in the shadow state *unconditionally*
/// and bound only when stereo is active, so the shipped shader has a sampler
/// slot it usually leaves dangling.
///
/// **Bucket 2 — a uniform branch**, and it is one bit: `TINTED`, plus the fog
/// mode every shader here carries. `TINTED` is `(nStage == 2) &&
/// !IS_PARAM_DEFINED( m_nPortalColorTexture )`, so it is decided by the
/// *absence* of a texture rather than by a switch — and exactly one material
/// in the game takes it.
///
/// **Bucket 3 — a real pipeline variant.** [`RenderState`], which for this
/// shader is a constant: see [`portal_refract_render_state`].
///
/// So `PortalRefract` needs **one** pipeline for the game's five stage-2
/// materials, which is the fewest of any shader here — `WorldVertexTransition`
/// is the only other one-pipeline shader and it has 18 materials. `TINTED` is a
/// uniform branch rather than a variant, and the pipeline key is
/// `(shader, RenderState, TargetFormat)` with a `RenderState` this shader does
/// not vary.
///
/// # `$PortalColorScale` is 4, and that is why an oval is bright
///
/// It multiplies the gradient after the lookup — Valve's comment is "Brighten
/// colors to make it look more emissive" — so a portal's colour leaves the
/// shader well above 1 and is then scaled back by the exposure. Both shipped
/// colours use 4.0; the tinted co-op material uses 1.0 and a gradient that
/// tops out at 0.3, which is a quarter of the brightness and is deliberate.
pub fn portal_refract_uniforms(vmt: &Vmt) -> PortalRefractUniforms {
    let kind = ShaderKind::PortalRefract;

    let mut flags = 0;
    if param_value(kind, vmt, "$nofog").is_some_and(|var| var.as_bool()) {
        flags |= PortalRefractFlags::NO_FOG;
    }
    // `int nTinted = ((nStage == 2) && !IS_PARAM_DEFINED( info.m_nPortalColorTexture )) ? 1 : 0;`
    // (`portal_refract_helper.cpp:73`). The stage is 2 by construction here.
    let has_color_texture = vmt
        .var("$portalcolortexture")
        .and_then(|var| var.as_str())
        .is_some_and(|name| !name.is_empty());
    if !has_color_texture {
        flags |= PortalRefractFlags::TINTED;
    }

    PortalRefractUniforms {
        texture_transform: {
            // `SetVertexShaderTextureTransform( CONST_1, m_nTextureTransform )`
            // — the same 2x4 every other shader here uploads, and the vertex
            // shader reads two of the four components. See the WGSL.
            let rows = param_value(kind, vmt, "$texturetransform")
                .map(|var| var.as_matrix())
                .unwrap_or(super::var::IDENTITY);
            [rows[0], rows[1]]
        },
        // `kDefaultPortalColorGradientDark`/`Light`
        // (`portal_refract_helper.h:20`), which agree with the declared
        // defaults for once.
        gradient_dark: init_vec(vmt, "$portalcolorgradientdark", [0.0, 0.0, 0.0, 1.0]),
        gradient_light: init_vec(vmt, "$portalcolorgradientlight", [1.0, 1.0, 1.0, 1.0]),
        params: [
            // `kDefaultPortalColorScale` is **1.0** and the declared default is
            // `"0.0"` — the two disagree and the code wins, as it does for
            // `Refract`'s `$localrefractdepth`. A 0 here would make every
            // portal black.
            init_float(vmt, "$portalcolorscale", 1.0),
            0.0,
            0.0,
            0.0,
        ],
        flags,
        _padding: [0; 3],
    }
}

/// `GammaToLinear` applied to a colour parameter's `rgb`, leaving `w` alone.
///
/// `CBaseVSShader::SetPixelShaderConstantGammaToLinear` (`BaseVSShader.cpp:138`)
/// and the mathlib function under it (`mathlib/color_conversion.cpp:276`). It
/// is not simply `pow( x, 2.2 )`, and all three of the differences are
/// load-bearing for a shipped Portal 2 material:
///
/// - **A component above 1 is passed through untouched**, which is the shader
///   helper's own `val > 1.0f ? val : GammaToLinear( val )`. That is what lets
///   content over-drive a tint past white.
/// - **A component at or above 0.95 becomes exactly 1.** `$refracttint
///   "{235 247 247}"` on sixteen `props_destruction` glass materials is
///   `[0.922 0.969 0.969]`, so two of its three channels clamp to 1 and only
///   the red is decoded — the tint the glass actually shows is
///   `[0.835 1 1]`, not `[0.835 0.933 0.933]`.
/// - **It is a 256-entry lookup table**, so the input is quantized to
///   `round( x * 255 ) / 255` first.
///
/// Note what does *not* use this: `VertexLitGeneric`'s `$envmaptint`, which
/// takes the *other* decode — [`gamma_to_linear_full_range_param`]. The two are
/// different functions reached through different helpers, and
/// `models/sabotage/glass01` is the shipped material that tells them apart.
fn gamma_to_linear_param(gamma: [f32; 4]) -> [f32; 4] {
    let convert = |value: f32| {
        if value > 1.0 {
            return value;
        }
        if value < 0.0 {
            return 0.0;
        }
        if value >= 0.95 {
            return 1.0;
        }
        ((value * 255.0).round() / 255.0).powf(2.2)
    };
    [
        convert(gamma[0]),
        convert(gamma[1]),
        convert(gamma[2]),
        gamma[3],
    ]
}

/// `GammaToLinearFullRange` applied to a colour parameter's `rgb`, leaving `w`
/// alone: the plain `pow( x, 2.2 )`, with no table and no clamping of any kind.
///
/// This is `SetEnvMapTintPixelShaderDynamicStateGammaToLinear`
/// (`public/shaderlib/commandbuilder.h:564`), which is how **`VertexLitGeneric`
/// and only `VertexLitGeneric`** gets its `$envmaptint` to the pixel shader
/// (`vertexlitgeneric_dx9_helper.cpp:1474`). `GammaToLinearFullRange` itself is
/// `mathlib/color_conversion.cpp:266` and is two lines long.
///
/// **It is not [`gamma_to_linear_param`]**, and the difference is deliberate in
/// the original rather than incidental. The command-builder helper used to read
/// `GetLinearVecValue` — the 256-entry table, with the `>= 0.95` clamp to 1 —
/// and there is a signed comment at `:571` saying why it was changed:
///
/// ```text
/// //this->Param( tintVar)->GetLinearVecValue( color, 3 );
/// // (wills) converted this line to the following so that envmaptint can be
/// // over-driven beyond 0-1 range
/// ```
///
/// So the clamp was removed *on purpose*, to let a material push a reflection
/// past white. `models/sabotage/glass01` is the one material in Portal 2 that
/// exercises it, with `$envmaptint "[5 5 5]"`: this decode makes that 34.5,
/// where [`gamma_to_linear_param`]'s `> 1` passthrough would leave it 5. It is
/// not reachable yet — that material's `$envmap` is `env_cubemap`, so
/// [`envmap_name`] refuses it and the reflection is off — but it becomes the
/// difference between the two functions the moment per-instance cubemaps land.
///
/// Three further consequences of "no clamping of any kind", all measured
/// against the shipped game rather than assumed:
///
/// - **Nothing is passed through.** A value just under 1 is decoded like any
///   other, so `[0.95 0.95 0.95]` is 0.893 here and exactly 1 under the table.
/// - **A dark tint collapses much harder than it looks.** Every one of the 57
///   `VertexLitGeneric` materials in Portal 2 with a resolvable `$envmap`
///   writes a tint, and every one of them is dark: `[0.05 0.05 0.05]` on 20 of
///   them becomes **0.0014**, a factor of 36, and `[0.01 0.01 0.01]` on five
///   more becomes 4.0e-5 — a factor of 251 — which is Valve asking for no reflection at all
///   rather than for a faint one.
/// - **A negative component is a NaN**, because `powf` has no domain guard and
///   neither does C's `pow`. Reproduced rather than clamped, because the two
///   agree exactly and no shipped material reaches it: of the 345 `.vmt` files
///   in the game that define `$envmaptint`, none has a negative component (the
///   census in `material.rs` counts them).
///
/// **`LightmappedGeneric` deliberately does not get this**, and that asymmetry
/// is Valve's: `lightmappedgeneric_dx9_helper.cpp:901` calls the *other*
/// command-builder overload (`commandbuilder.h:552`), which sends the tint to
/// the shader in gamma space with no decode at all. 135 shipped materials are
/// affected by that and will be when this module's `$envmap` support reaches
/// the world path — so the symmetrical-looking "fix" of calling this from
/// `lightmapped_uniforms` would be a divergence, not a correction.
///
/// **Not ported, and the reason both helpers have an `else` branch**: the tint
/// is replaced by black when `mat_specular` is 0 or `mat_fullbright` is 2
/// (`g_pConfig->bShowSpecular`, `g_pConfig->nFullbright`), which is how Source
/// turns every reflection in the game off at once. Neither cvar exists in this
/// port; both default to the branch taken here.
fn gamma_to_linear_full_range_param(gamma: [f32; 4]) -> [f32; 4] {
    let convert = |value: f32| value.powf(2.2);
    [
        convert(gamma[0]),
        convert(gamma[1]),
        convert(gamma[2]),
        gamma[3],
    ]
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
/// # The value stays in gamma space, and for the pixel shaders that is a gap
///
/// **This is a known divergence, found while porting [`Phong`](ShaderKind::Phong)
/// and deliberately left for its own change**, because fixing it moves every
/// tinted model in the game and wants its own verification.
///
/// Valve writes the modulation into *two* registers with *two* different
/// transforms. `cModulationColor` (VS `c47`) is gamma, which is what this
/// returns. `g_DiffuseModulation` (PS `c1`) depends on which
/// `PI_SetModulationPixelShaderDynamicState*` a shader emits, and the live
/// handler for the one the model shaders emit is
/// `GammaToLinearExtendedSIMD( color2 * instanceModulation )`
/// (`shaderapidx8.cpp:8664`, under `USE_OLD_GAMMA == false`) — so **the pixel
/// shader's copy is linear**. `VertexLitGeneric` reaches it whenever
/// `bSRGBWrite` (`vertexlitgeneric_dx9_helper.cpp:654`, and this port's frame
/// buffer is sRGB), and `Phong` reaches it unconditionally
/// (`phong_dx9_helper.cpp:626`). `LightmappedGeneric` emits the *gamma*
/// variant (`lightmappedgeneric_dx9_helper.cpp:830`), so the asymmetry is
/// three-way again and this value is right for the world path.
///
/// A previous reading of `ApplyColor2Factor` (`BaseShader.cpp:731`) concluded
/// the two could not both be right; they can, because they are two registers.
/// Reversing it is one call to [`gamma_to_linear_param`]-shaped arithmetic
/// applied only for the model shaders, plus a check of what
/// `$color`/`$color2`/`$alpha` content expects.
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

/// The two things the shadow phase asks a texture that is already loaded.
///
/// Not the texture itself, deliberately: what `TextureIsTranslucent` and
/// `GetActualWidth`/`GetActualHeight` want is two facts, and taking them as
/// facts is what keeps this module's tests runnable without a GPU — a
/// [`Texture`] cannot exist without a `wgpu::Device`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureFacts {
    /// `ITexture::GetActualWidth`.
    pub width: u32,
    /// `ITexture::GetActualHeight`.
    pub height: u32,
    /// `ITexture::IsTranslucent` — whether the `.vtf` claims an alpha channel.
    pub translucent: bool,
}

impl TextureFacts {
    pub fn of(texture: &Texture) -> TextureFacts {
        TextureFacts {
            width: texture.width,
            height: texture.height,
            translucent: texture.is_translucent(),
        }
    }
}

/// The textures the shadow phase has to have already resolved.
///
/// The shadow phase cannot decide blending from the `.vmt` alone — Valve's
/// `TextureIsTranslucent` asks a loaded `ITexture` whether it has an alpha
/// channel — so [`render_state`] takes the answers as a parameter. It is a
/// struct with two fields rather than one, because **different shaders ask
/// about different textures**: everything derived from the shared
/// `UnlitGeneric`/`VertexLitGeneric` helper asks about `$basetexture`, and
/// `Refract` asks about `$normalmap`.
///
/// [`Material::new`](super::material::Material::new) fills whichever fields
/// the shader's [`texture_requests`] produced; a field left `None` reads as
/// "opaque", which is the same answer the standard white texture and the error
/// checkerboard give, since neither is created with an alpha flag.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolvedTextures {
    /// `$basetexture`.
    pub base: Option<TextureFacts>,
    /// `$normalmap` — `Refract`'s, and the one whose alpha decides its
    /// blending.
    pub normal_map: Option<TextureFacts>,
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
/// # Alpha modulation, and why there are two of these
///
/// `bIsAlphaModulating` is a *draw-time* input in the original: it comes from
/// the instance's diffuse modulation alpha (`shaderapidx8.cpp:4944`), which is
/// `$alpha` times whatever the renderable's own modulation supplied. That is
/// why a material carried up to eight state snapshots, indexed by
/// `SHADER_USING_ALPHA_MODULATION` and friends.
///
/// This is the snapshot with that bit **clear**;
/// [`render_state_modulated`] is the one with it set, and
/// [`Material::state_for`](super::material::Material::state_for) picks between
/// them from the final modulation alpha exactly as `CShaderAPIDx8::DrawMesh2`
/// (`shaderapidx8.cpp:4907`) does. The `$alpha`
/// parameter is read here as well, because the product Valve tests already has
/// it in: a material with `$alpha 0.5` is alpha-modulating whoever draws it.
pub fn render_state(kind: ShaderKind, vmt: &Vmt, textures: ResolvedTextures) -> RenderState {
    render_state_with_modulation(kind, vmt, textures, false)
}

/// [`render_state`] with `SHADER_USING_ALPHA_MODULATION` set — the snapshot a
/// draw whose modulation alpha is not 1 uses.
///
/// It is not simply "the same with blending turned on": `IsAlphaModulating()`
/// is one of the three terms `EvaluateBlendRequirements` ORs, so the *result*
/// still goes through `$additive`'s fork, still turns depth writes off, still
/// loses alpha writes, and is still overridden wholesale by `$multiply`. One
/// call, four consequences, which is why this re-runs the shadow phase rather
/// than patching [`RenderState::blend`].
pub fn render_state_modulated(
    kind: ShaderKind,
    vmt: &Vmt,
    textures: ResolvedTextures,
) -> RenderState {
    render_state_with_modulation(kind, vmt, textures, true)
}

fn render_state_with_modulation(
    kind: ShaderKind,
    vmt: &Vmt,
    textures: ResolvedTextures,
    alpha_modulating: bool,
) -> RenderState {
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

    // `Refract` diverges from here on, so it returns rather than falling
    // through: see [`refract_render_state`].
    if kind == ShaderKind::Refract {
        return refract_render_state(vmt, textures, state, alpha_modulating);
    }
    // And so does `PortalRefract`, for the opposite reason: where `Refract`'s
    // blending is decided by content, this shader's is decided by nothing at
    // all. See [`portal_refract_render_state`].
    if kind == ShaderKind::PortalRefract {
        return portal_refract_render_state(state);
    }

    // --- EvaluateBlendRequirements ---------------------------------------
    let alpha_test = flags.contains(MaterialFlags::ALPHATEST);
    // `IsAlphaModulating()` (`shaderlib/BaseShader.cpp:656`) is the modulation
    // flag alone; `$alpha` is here because the draw-time product Valve tests
    // has it folded in already.
    let alpha_modulating =
        alpha_modulating || param_value(kind, vmt, "$alpha").is_some_and(|var| var.as_f32() != 1.0);
    let translucent = alpha_modulating
        || flags.contains(MaterialFlags::VERTEXALPHA)
        || (base_texture_is_translucent(vmt, textures.base) && !alpha_test);

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
    //
    // **`Phong` is excluded for the same reason and it is a different gap.**
    // `DrawPhong_DX9`'s shadow block has no `MATERIAL_VAR_MULTIPLY` case at
    // all, so a material that reaches Phong loses `$multiply` even though the
    // shader it named would have honoured it. Measured: **none of the game's
    // 317 Phong materials sets it**, so the gap is invisible on shipped
    // content.
    if !matches!(
        kind,
        ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition | ShaderKind::Phong
    ) && flags.contains(MaterialFlags::MULTIPLY)
    {
        state.blend = BlendMode::Multiply;
        state.depth_write = false;
    }

    state
}

/// `Refract`'s half of the shadow phase, after
/// `SetInitialShadowState` has run.
///
/// `DrawRefract_DX9`'s `SHADOW_STATE` block (`refract_dx9_helper.cpp:141`).
/// Three things in it are unlike every other shader in the set, and the first
/// two are why this is a separate function rather than two more `if`s in
/// [`render_state`]:
///
/// 1. **Blending is decided from the `$normalmap`, not the `$basetexture`** —
///    `SetDefaultBlendingShadowState( info.m_nNormalMap, false )` — because
///    the base texture here is an image being warped rather than the surface's
///    own colour. And the `isBaseTexture` argument being `false` matters: it
///    skips the whole `$selfillum`/`$basealphaenvmapmask`/`$translucent`
///    reconciliation that [`base_texture_is_translucent`] does and asks the
///    `.vtf` directly.
/// 2. **It is only decided at all when the material has no `$envmap`**, so a
///    reflective refractor gets `SetInitialShadowState`'s blending — which is
///    *none*. Measured: **all 29 of the game's `$model 1` `Refract` materials
///    name an `$envmap`**, so every piece of refracting glass in Portal 2
///    draws opaque and the refraction it shows is the copy of the frame it
///    sampled, not a blend with the frame it is writing. The ten that would
///    blend are the `materials/particle/` warps, and the particle system is
///    not ported.
/// 3. **`EnableAlphaWrites( bFullyOpaque )` reads a blend type that was never
///    applied.** `bFullyOpaque` comes from
///    `EvaluateBlendRequirements( BASETEXTURE, true )` (`:136`) — the *base*
///    texture, the test point 1 just said this shader does not use for
///    blending — and is then narrowed by `$masked` and by whether the normal
///    map is translucent. So the alpha write mask and the blend mode are
///    computed from two different textures. Valve's, and reproduced: it is a
///    write mask, and the only thing that reads the frame's alpha channel is
///    the underwater pass, which is not ported.
fn refract_render_state(
    vmt: &Vmt,
    textures: ResolvedTextures,
    mut state: RenderState,
    alpha_modulating: bool,
) -> RenderState {
    let kind = ShaderKind::Refract;
    let flags = vmt.flags;
    let defined = |name| {
        vmt.var(name)
            .and_then(|var| var.as_str())
            .is_some_and(|value| !value.is_empty())
    };

    // `EnableDepthWrites( bWriteZ )`, where `bWriteZ` is `$nowritez == 0`.
    // Zero of the game's 37 materials set it; it is one line and honest to
    // read, so the parameter table can keep promising it does something.
    if param_value(kind, vmt, "$nowritez").is_some_and(|var| var.as_bool()) {
        state.depth_write = false;
    }

    // `TextureIsTranslucent( m_nNormalMap, false )` — the `.vtf`'s own answer,
    // nothing else. Measured: `glass/refract_light_normal` is DXT1 with no
    // alpha flag, so the six glass materials in `sp_a1_intro1` are *not*
    // translucent by this test even though they are glass.
    let normal_map_is_translucent = textures.normal_map.is_some_and(|facts| facts.translucent);
    let alpha_test = flags.contains(MaterialFlags::ALPHATEST);
    let alpha_modulating =
        alpha_modulating || param_value(kind, vmt, "$alpha").is_some_and(|var| var.as_f32() != 1.0);

    if defined("$normalmap") && envmap_name(vmt).is_none() {
        let translucent = alpha_modulating
            || flags.contains(MaterialFlags::VERTEXALPHA)
            || (normal_map_is_translucent && !alpha_test);
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
        // `EnableAlphaBlending` turns depth writes off as well as blending on.
        if state.blend != BlendMode::None {
            state.depth_write = false;
        }
    }

    // `bFullyOpaque`: the blend type the *base* texture would have asked for,
    // narrowed twice. `$masked` is bucket 1 and therefore always false here.
    let base_blend_translucent = alpha_modulating
        || flags.contains(MaterialFlags::VERTEXALPHA)
        || (base_texture_is_translucent(vmt, textures.base) && !alpha_test);
    state.write_alpha = !base_blend_translucent && !alpha_test && !normal_map_is_translucent;

    state
}

/// `PortalRefract`'s half of the shadow phase, after `SetInitialShadowState`
/// has run — and it takes **no arguments but the state**, because nothing
/// about it depends on the material.
///
/// `DrawPortalRefract`'s `SHADOW_STATE` block (`portal_refract_helper.cpp:83`)
/// for `nStage == 2`, which is four unconditional calls:
///
/// | | |
/// |---|---|
/// | `EnableAlphaBlending( SRC_ALPHA, ONE_MINUS_SRC_ALPHA )` | [`BlendMode::Blend`] |
/// | `EnableDepthWrites( false )` | for every stage but 1 |
/// | `EnableAlphaWrites( false )` | already the default state's |
/// | `EnablePolyOffset( SHADER_POLYOFFSET_DECAL )` | [`DepthBias::Decal`] |
///
/// # Two things this does *not* do that every other shader here does
///
/// **It never asks a texture whether it is translucent.**
/// `EvaluateBlendRequirements` is not called at all — the blend is a literal —
/// so `$translucent`, `$additive`, `$vertexalpha`, `$alpha` and the base
/// texture's alpha channel all reach nothing. `effects/fakeportalring_blue`
/// writes `$translucent 1` and gets exactly the state it would have got
/// without it.
///
/// **So [`render_state_modulated`] is identical to [`render_state`] here**,
/// which makes this the first shader in the port whose two snapshots are the
/// same object. That is not a shortcut: `SHADER_USING_ALPHA_MODULATION`'s only
/// effect anywhere is to turn blending on, and it is already on.
///
/// # The polygon offset is load-bearing
///
/// A portal's quad is drawn **exactly on** the wall it is stuck to —
/// `DrawSimplePortalMesh` takes a `fForwardOffsetModifier` and then throws it
/// away, over a comment that says the offset moved into the shaders
/// (`portalrenderable_flatbasic.cpp:1226`). Without the decal bias the quad
/// z-fights with the wall; with it, and with depth writes off, it wins
/// everywhere and writes nothing.
fn portal_refract_render_state(mut state: RenderState) -> RenderState {
    state.blend = BlendMode::Blend;
    state.depth_write = false;
    state.write_alpha = false;
    state.depth_bias = DepthBias::Decal;
    state
}

/// `CBaseShader::TextureIsTranslucent( BASETEXTURE, true )`
/// (`shaderlib/BaseShader.cpp:605`).
///
/// Not simply "does the texture have alpha": the base texture's alpha channel
/// is *shared*, and three flags claim it for something other than translucency.
/// If any of them does, the material is opaque no matter what the `.vtf`
/// contains.
fn base_texture_is_translucent(vmt: &Vmt, base_texture: Option<TextureFacts>) -> bool {
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
    texture.translucent
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

    fn refract_vmt(body: &str) -> Vmt {
        let text = format!("\"Refract\" {{ {body} }}");
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        Vmt::from_keyvalues("test.vmt", &document).expect("a shader block")
    }

    fn portal_refract_vmt(body: &str) -> Vmt {
        let text = format!("\"PortalRefract\" {{ {body} }}");
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        Vmt::from_keyvalues("test.vmt", &document).expect("a shader block")
    }

    /// `models/portals/portalstaticoverlay_1.vmt`, verbatim from the shipped
    /// file minus its `<DX90` fallback block and its three proxies.
    const PORTAL_OVERLAY_1: &str = r#"
        "$Stage" "2"
        "$PortalOpenAmount" "0.0"
        "$PortalStatic" "0.0"
        "$PortalMaskTexture" "models/portals/noise-blur-256x256"
        "$PortalColorTexture" "models/portals/portal-blue-color"
        "$PortalColorScale" "4.0"
        "$time" "0.0"
    "#;

    /// **Only `$Stage 2` resolves**, and the two that do not are the whole
    /// reason `resolve` can answer `None` for a name `from_name` knows.
    #[test]
    fn portal_refract_resolves_only_its_third_stage() {
        assert_eq!(
            ShaderKind::from_name("PortalRefract"),
            Some(ShaderKind::PortalRefract),
            "content names it, unlike Phong"
        );
        assert_eq!(
            ShaderKind::resolve(&portal_refract_vmt(PORTAL_OVERLAY_1)),
            Some(ShaderKind::PortalRefract)
        );
        // `portal_refract_1.vmt` and `portal_stencil_hole.vmt`: the two
        // materials in the game that belong to the recursive view.
        assert_eq!(
            ShaderKind::resolve(&portal_refract_vmt("\"$Stage\" \"0\"")),
            None,
            "the see-through warp reads the scene and is not ported"
        );
        assert_eq!(
            ShaderKind::resolve(&portal_refract_vmt("\"$Stage\" \"1\"")),
            None,
            "the stencil punch needs a stencil"
        );
        // An undefined `$Stage` is 0, which `InitParamsPortalRefract` writes
        // back into the parameter.
        assert_eq!(ShaderKind::resolve(&portal_refract_vmt("")), None);
        // **`$UseOnStaticProp` forces stage 2**, whatever `$Stage` says — and
        // it is what makes `effects/fakeportalring_*` stage-2 materials.
        assert_eq!(
            ShaderKind::resolve(&portal_refract_vmt(
                "\"$Stage\" \"0\" \"$UseOnStaticProp\" \"1\""
            )),
            Some(ShaderKind::PortalRefract)
        );
    }

    /// The oval's pipeline state is a **constant**: no texture is consulted, no
    /// flag reaches it, and the modulated snapshot is the same object.
    #[test]
    fn the_portal_overlay_blends_whatever_its_vmt_says() {
        let kind = ShaderKind::PortalRefract;
        let none = ResolvedTextures {
            base: None,
            normal_map: None,
        };
        for body in [
            PORTAL_OVERLAY_1,
            // `effects/fakeportalring_blue`'s flags, which would change any
            // other shader's blending.
            "\"$Stage\" \"2\" \"$translucent\" \"1\" \"$basetexture\" \"models/portals/dummy-blue\"",
            "\"$Stage\" \"2\" \"$additive\" \"1\" \"$alpha\" \"0.25\"",
        ] {
            let vmt = portal_refract_vmt(body);
            let state = render_state(kind, &vmt, none);
            assert_eq!(state.blend, BlendMode::Blend, "{body}");
            assert!(!state.depth_write, "depth writes are off for every stage");
            assert!(!state.write_alpha);
            assert_eq!(
                state.depth_bias,
                DepthBias::Decal,
                "the quad is flush with the wall it is stuck to"
            );
            assert!(state.cull, "a portal is one-sided");
            assert_eq!(
                render_state_modulated(kind, &vmt, none),
                state,
                "SHADER_USING_ALPHA_MODULATION only turns blending on, and it is on"
            );
        }
    }

    /// `TINTED` is decided by the *absence* of `$PortalColorTexture`, and
    /// `$PortalColorScale`'s runtime default is 1 where its declared default
    /// is 0 — which would make every portal black.
    #[test]
    fn the_portal_overlays_gradient_comes_from_a_texture_or_two_colours() {
        let with_texture = portal_refract_uniforms(&portal_refract_vmt(PORTAL_OVERLAY_1));
        assert_eq!(with_texture.flags & PortalRefractFlags::TINTED, 0);
        assert_eq!(with_texture.params[0], 4.0, "$PortalColorScale");

        // `portalstaticoverlay_tinted.vmt`: no colour texture outside its
        // `<DX90` block, which this port does not read.
        let tinted = portal_refract_uniforms(&portal_refract_vmt(
            r#"
            "$Stage" "2"
            "$PortalMaskTexture" "models/portals/noise-blur-256x256"
            "$PortalColorGradientDark" "[0.0 0.0 0.0]"
            "$PortalColorGradientLight" "[0.3 0.3 0.3]"
            "$PortalColorScale" "1.0"
            "#,
        ));
        assert_ne!(tinted.flags & PortalRefractFlags::TINTED, 0);
        assert_eq!(tinted.gradient_light[0..3], [0.3, 0.3, 0.3]);
        assert_eq!(tinted.params[0], 1.0);

        // Nothing written at all: the *code's* default, not the table's.
        let bare = portal_refract_uniforms(&portal_refract_vmt("\"$Stage\" \"2\""));
        assert_eq!(
            bare.params[0], 1.0,
            "kDefaultPortalColorScale is 1, and the declared default of 0 would be black"
        );
        assert_eq!(
            bare.texture_transform,
            [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            "the identity transform, as two rows"
        );
    }

    /// It reads its own three numbers out of group 3, and nothing else: no
    /// lighting, no copy of the frame, and the smallest vertex layout in the
    /// set.
    #[test]
    fn the_portal_overlay_reads_its_open_amount_and_no_lighting() {
        let kind = ShaderKind::PortalRefract;
        assert_eq!(kind.context_binding(), Some(ContextBinding::PortalOverlay));
        let vmt = portal_refract_vmt(PORTAL_OVERLAY_1);
        assert_eq!(lighting(kind, &vmt), Lighting::None);
        assert!(
            !needs_frame_buffer_copy(kind, &vmt),
            "stage 0's, not this one"
        );
        // Position and one texture coordinate is all the stage-2 shader reads.
        assert_eq!(kind.vertex_layout(), VertexLayout::Simple);
        // Two textures, and **no `$basetexture`** — `fakeportalring_blue`
        // names one and stage 2 never samples it.
        let requests = texture_requests(kind, &vmt);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.param != "$basetexture"));
        assert_eq!(
            requests[0].color_space,
            super::super::ColorSpace::Linear,
            "a mask"
        );
        assert_eq!(
            requests[1].color_space,
            super::super::ColorSpace::Srgb,
            "a colour"
        );
    }

    /// `glass/container_window_warm`, the material three of `sp_a1_intro1`'s
    /// static props wear, verbatim from the shipped file minus its `<dx90`
    /// fallback block.
    const CONTAINER_WINDOW_WARM: &str = r#"
        "$model" "1"
        "$refractamount" ".025"
        "$bluramount" "1"
        "$REFRACTTINT" "[1.0 .89 .81]"
        "$normalmap" "glass/refract_light_normal"
        "$localrefract" "1"
        "$localrefractdepth" "0.025"
        "$basetexture" "glass/refract_light_color_container"
        "$envmap" "env_cubemap"
        "$envmapcontrast" "0"
        "$envmapsaturation" "[1 1 1]"
        "$envmaptint" "[.4 .4 .4]"
    "#;

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
        // **`Phong` is not a shader name at all.** There is no
        // `SHADER( Phong )` in `stdshaders/` and no `DEFINE_FALLBACK_SHADER`
        // for it; the materials that draw with it name `VertexLitGeneric` and
        // are redirected by `WantsPhongShader`. So this stays `None` and
        // `ShaderKind::resolve` is the thing that answers `Phong` — see
        // `a_phong_material_resolves_to_phong_and_is_not_nameable`.
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
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(""),
            ResolvedTextures::default(),
        );
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
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$ignorez" "1""#),
            ResolvedTextures::default(),
        );
        assert!(!state.depth_test && !state.depth_write);

        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$nocull" "1""#),
            ResolvedTextures::default(),
        );
        assert!(!state.cull);

        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$znearer" "1""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.depth_func, DepthFunc::Nearer);

        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$decal" "1""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.depth_bias, DepthBias::Decal);
        assert!(!state.depth_write, "a decal must not write depth");
    }

    #[test]
    fn alpha_modulation_alone_makes_a_material_translucent() {
        // No texture, no $translucent — just an alpha below one, which is what
        // `EvaluateBlendRequirements` checks first.
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$alpha" "0.5""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::Blend);
        assert!(!state.depth_write, "blending turns depth writes off");
        assert!(!state.write_alpha);

        // And exactly one is opaque.
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$alpha" "1""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::None);
    }

    #[test]
    fn additive_and_multiply_pick_their_blend_modes() {
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$additive" "1""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::Add);

        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$additive" "1" "$alpha" "0.5""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::BlendAdd);

        // `$multiply` is applied last and overrides whatever came before.
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$multiply" "1" "$additive" "1""#),
            ResolvedTextures::default(),
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
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::Multiply);
        assert!(!state.write_alpha);
    }

    /// The second snapshot: `SHADER_USING_ALPHA_MODULATION` set, which is what
    /// an instance drawn at `renderamt 200` selects.
    #[test]
    fn the_alpha_modulated_snapshot_is_the_shadow_phase_run_again() {
        // A perfectly ordinary opaque material.
        let opaque = vmt(r#""$basetexture" "wall""#);
        assert_eq!(
            render_state(
                ShaderKind::UnlitGeneric,
                &opaque,
                ResolvedTextures::default()
            )
            .blend,
            BlendMode::None
        );

        let modulated = render_state_modulated(
            ShaderKind::UnlitGeneric,
            &opaque,
            ResolvedTextures::default(),
        );
        assert_eq!(modulated.blend, BlendMode::Blend);
        assert!(
            !modulated.depth_write,
            "`EnableAlphaBlending` turns depth writes off too"
        );
        assert!(!modulated.write_alpha);

        // It is not "blending on": every other term of the shadow phase still
        // runs against it. `$additive` forks the result...
        assert_eq!(
            render_state_modulated(
                ShaderKind::UnlitGeneric,
                &vmt(r#""$additive" "1""#),
                ResolvedTextures::default(),
            )
            .blend,
            BlendMode::BlendAdd
        );
        // ...and `$multiply` still overrides it wholesale.
        assert_eq!(
            render_state_modulated(
                ShaderKind::UnlitGeneric,
                &vmt(r#""$multiply" "1""#),
                ResolvedTextures::default(),
            )
            .blend,
            BlendMode::Multiply
        );
        // The flags that have nothing to do with blending are untouched.
        let decal = render_state_modulated(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$decal" "1" "$nocull" "1""#),
            ResolvedTextures::default(),
        );
        assert_eq!(decal.depth_bias, DepthBias::Decal);
        assert!(!decal.cull);
    }

    /// `Refract`'s own shadow phase honours the modulation flag too — and only
    /// where its own rules let it, which is the branch no shipped material
    /// takes.
    #[test]
    fn refract_takes_alpha_modulation_only_without_an_envmap() {
        // `$normalmap` and no `$envmap`: the branch that decides blending.
        let state = render_state_modulated(
            ShaderKind::Refract,
            &refract_vmt(r#""$normalmap" "glass/refract_normal""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::Blend);

        // With an `$envmap` the whole blend evaluation is skipped, so an
        // alpha-modulated draw of one still draws opaque. All 29 of the game's
        // `$model 1` refracting materials are here.
        //
        // `$envmap "env_cubemap"` is deliberately *not* used for this: it names
        // no file, so [`envmap_name`] answers `None` and this branch is taken —
        // where Valve's `bHasEnvmap` is `params[ENVMAP]->IsTexture()`
        // (`refract_dx9_helper.cpp:127`) and the instance cubemap is a texture,
        // so it would not be. That reduction is [`envmap_name`]'s and predates
        // the blended pass; it becomes visible the day the per-instance cubemap
        // is bound.
        let state = render_state_modulated(
            ShaderKind::Refract,
            &refract_vmt(r#""$normalmap" "glass/refract_normal" "$envmap" "metal/shiny""#),
            ResolvedTextures::default(),
        );
        assert_eq!(state.blend, BlendMode::None);
    }

    #[test]
    fn alpha_tested_materials_are_opaque_and_discard() {
        let state = render_state(
            ShaderKind::UnlitGeneric,
            &vmt(r#""$translucent" "0" "$alphatest" "1""#),
            ResolvedTextures::default(),
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
        // Three 2x4 transforms, seven vec4s, then four words.
        assert_eq!(size_of::<PhongUniforms>(), 3 * 32 + 7 * 16 + 16);
        assert_eq!(size_of::<PhongUniforms>() % 16, 0);
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
    fn a_phong_material_resolves_to_phong_and_is_not_nameable() {
        // `WantsPhongShaderInternal` (`vertexlitgeneric_dx9_helper.cpp:70`),
        // which decides whether a `VertexLitGeneric` `.vmt` is really drawn by
        // `Phong` — 317 of the game's 1,135 of them.

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

        // `resolve` is the redirect built on the predicate, and it is the only
        // way to reach `ShaderKind::Phong`: no `.vmt` names it, and
        // `from_name` says so (`unknown_shader_names_do_not_resolve`).
        let phong = model_vmt(r#""$phong" "1" "$bumpmap" "x""#);
        assert_eq!(ShaderKind::resolve(&phong), Some(ShaderKind::Phong));
        assert_eq!(
            ShaderKind::from_name(&phong.shader),
            Some(ShaderKind::VertexLitGeneric),
            "the name the content wrote is unchanged"
        );
        let plain = model_vmt(r#""$bumpmap" "x""#);
        assert_eq!(
            ShaderKind::resolve(&plain),
            Some(ShaderKind::VertexLitGeneric)
        );
        // The redirect is `VertexLitGeneric`'s alone. `$phong` on a world
        // material reaches `DrawLightmappedGeneric_DX9`, which has its own
        // unported phong path and no dispatch.
        let world = {
            let text = r#""LightmappedGeneric" { "$phong" "1" "$bumpmap" "x" }"#;
            let document = keyvalues::parse("test.vmt", text).expect("valid keyvalues");
            Vmt::from_keyvalues("test.vmt", &document).expect("a shader block")
        };
        assert_eq!(
            ShaderKind::resolve(&world),
            Some(ShaderKind::LightmappedGeneric)
        );
    }

    fn phong_vmt(body: &str) -> Vmt {
        let text = format!(r#""VertexLitGeneric" {{ "$phong" "1" "$bumpmap" "b" {body} }}"#);
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        let vmt = Vmt::from_keyvalues("test.vmt", &document).expect("a shader block");
        assert_eq!(
            ShaderKind::resolve(&vmt),
            Some(ShaderKind::Phong),
            "the fixture has to actually reach Phong"
        );
        vmt
    }

    #[test]
    fn phong_declares_its_own_parameters_and_shares_the_dispatch_ones() {
        // Valve has one `BEGIN_SHADER_PARAMS` block for both shaders; this
        // module splits it, because a table entry is a promise that setting
        // the parameter does something and on a non-phong material it does
        // not. See `PHONG_PARAMS`.
        let declared = |kind: ShaderKind, name: &str| kind.param(name).is_some();

        for name in ["$phongboost", "$phongexponent", "$rimlight", "$rimmask"] {
            assert!(declared(ShaderKind::Phong, name), "Phong declares {name}");
            assert!(
                !declared(ShaderKind::VertexLitGeneric, name),
                "VertexLitGeneric does not declare {name}"
            );
        }
        // The three dispatch parameters are the exception: `wants_phong` reads
        // them through `VertexLitGeneric` before there is a `Phong` to read
        // them through, so both tables carry them.
        for name in ["$phong", "$basemapalphaphongmask", "$lightwarptexture"] {
            assert!(declared(ShaderKind::Phong, name), "Phong declares {name}");
            assert!(
                declared(ShaderKind::VertexLitGeneric, name),
                "VertexLitGeneric declares {name}"
            );
        }
        // And everything the shared table has is still reachable from Phong,
        // because Valve's declaration is one block.
        for name in [
            "$basetexture",
            "$bumpmap",
            "$detail",
            "$envmap",
            "$selfillummask",
        ] {
            assert!(declared(ShaderKind::Phong, name), "Phong declares {name}");
        }
    }

    #[test]
    fn phong_half_lambert_is_on_unless_the_material_opts_out() {
        // The third CS:GO-shaped default, and the sharpest: `bPhongHalfLambert`
        // is hard-coded `false` over a commented-out read of
        // `$phongdisablehalflambert`, and the parameter's own declaration says
        // half-Lambert "has always been forced on in phong". 26 shipped
        // materials write it and 20 write `1`, which would be a no-op against
        // an off-by-default.
        let half_lambert =
            |body: &str| phong_uniforms(&phong_vmt(body)).flags & PhongFlags::HALF_LAMBERT != 0;
        assert!(half_lambert(""), "on by default");
        assert!(!half_lambert(r#""$phongdisablehalflambert" "1""#));
        assert!(half_lambert(r#""$phongdisablehalflambert" "0""#));
        // **Not the `$halflambert` material flag**, which is what
        // `VertexLitGeneric` reads and which this shader never looks at. 25 of
        // the 317 set it and get nothing.
        assert!(half_lambert(r#""$halflambert" "1""#));
        assert_eq!(
            vertex_lit_uniforms(&model_vmt(r#""$halflambert" "1""#)).flags
                & VertexLitFlags::HALF_LAMBERT,
            VertexLitFlags::HALF_LAMBERT,
            "the flag still means something to the other shader"
        );
    }

    #[test]
    fn the_phong_envmap_tint_is_not_gamma_decoded() {
        // Three shaders, three answers, and this is the third.
        // `DrawPhong_DX9:800` reads `$envmaptint` with a plain `GetVecValue`;
        // `DrawVertexLitGeneric_DX9` runs it through `GammaToLinearFullRange`
        // and `Refract` through the 256-entry table. Decoding it here would
        // darken every Phong reflection in the game by up to a factor of 36.
        let body = r#""$envmap" "metal/e" "$envmaptint" "[0.05 0.05 0.05]""#;
        let phong = phong_uniforms(&phong_vmt(body));
        assert_eq!(&phong.envmap_tint[..3], &[0.05, 0.05, 0.05]);

        let lit = vertex_lit_uniforms(&model_vmt(body));
        assert!(
            (lit.envmap_tint[0] - 0.0013732).abs() < 1e-6,
            "the other shader decodes: {}",
            lit.envmap_tint[0]
        );
    }

    #[test]
    fn the_phong_exponent_falls_back_to_the_map_through_a_zero_sentinel() {
        // "If the exponent passed in as a constant is zero, use the value from
        // the map as the exponent" (`phong_ps20b.fxc:700`). 71 of the 317
        // materials have an exponent texture and no `$phongexponent`, so the
        // sentinel is the live path rather than a fallback.
        let exponent = |body: &str| phong_uniforms(&phong_vmt(body)).shader_controls2[2];
        assert_eq!(exponent(""), 0.0, "unset means read the map");
        assert_eq!(exponent(r#""$phongexponent" "50""#), 50.0);
        // Valve's own overloading, reproduced: an explicit zero is the
        // sentinel too.
        assert_eq!(exponent(r#""$phongexponent" "0""#), 0.0);
    }

    #[test]
    fn the_specular_tint_defaults_and_the_albedo_path_needs_a_map() {
        // `float vSpecularTint[4] = {1, 1, 1, 4}`, whose `w` is the rim
        // exponent's default. A zero tint means "there was no constant tint in
        // the vmt", and what happens next depends on whether there is an
        // exponent map to tint *from*.
        let tint = |body: &str| phong_uniforms(&phong_vmt(body)).specular_rim;

        assert_eq!(tint(""), [1.0, 1.0, 1.0, 4.0]);
        assert_eq!(
            tint(r#""$phongtint" "[0.85 0.85 1]""#),
            [0.85, 0.85, 1.0, 4.0]
        );
        // A zero tint with nothing to read falls back to white.
        assert_eq!(
            tint(r#""$phongtint" "[0 0 0]""#),
            [1.0, 1.0, 1.0, 4.0],
            "all four shipped zero-tint materials take this branch"
        );
        // With an exponent map *and* `$phongalbedotint`, `x` becomes the -1
        // sentinel that switches the shader to tinting with the albedo. No
        // Portal 2 material does both.
        assert_eq!(
            tint(r#""$phongtint" "[0 0 0]" "$phongexponenttexture" "e" "$phongalbedotint" "1""#)[0],
            -1.0
        );
        // `$phongalbedotint` without a map is not enough — the map is where
        // the per-texel tint amount lives.
        assert_eq!(
            tint(r#""$phongtint" "[0 0 0]" "$phongalbedotint" "1""#),
            [1.0, 1.0, 1.0, 4.0]
        );
    }

    #[test]
    fn the_rim_exponent_shares_a_register_and_is_clamped_to_one() {
        // `vSpecularTint[3] = MAX( $rimlightexponent, 1.0f )`, and only when
        // rim lighting is actually on. Three of the seven shipped values are
        // below 1 — 0.2 on seventeen materials — so the clamp decides what
        // nineteen of them look like.
        let w = |body: &str| phong_uniforms(&phong_vmt(body)).specular_rim[3];
        assert_eq!(w(r#""$rimlight" "1" "$rimlightexponent" "20""#), 20.0);
        assert_eq!(w(r#""$rimlight" "1" "$rimlightexponent" "0.2""#), 1.0);
        // Without `$rimlight` the parameter is inert and the default stands.
        // 54 materials set an exponent and only 39 set `$rimlight`.
        assert_eq!(w(r#""$rimlightexponent" "20""#), 4.0);
        // `$rimlight 0.8` is *off*, because the test is `GetIntValue() != 0`
        // and that truncates. One shipped material writes it.
        assert_eq!(w(r#""$rimlight" "0.8" "$rimlightexponent" "20""#), 4.0);
        assert_eq!(
            phong_uniforms(&phong_vmt(r#""$rimlight" "0.8""#)).flags & PhongFlags::RIMLIGHT,
            0
        );
    }

    #[test]
    fn one_register_is_the_detail_blend_factor_or_the_albedo_boost() {
        // `flBlendFactorOrPhongAlbedoBoost` (`phong_dx9_helper.cpp:610`): a
        // material with a `$detail` cannot also have a `$phongalbedoboost`,
        // because they are the same float. Free on content — nothing in
        // Portal 2 sets the boost.
        let w = |body: &str| phong_uniforms(&phong_vmt(body)).selfillum_tint[3];
        assert_eq!(w(r#""$phongalbedoboost" "3""#), 3.0);
        assert_eq!(w(r#""$detail" "d" "$detailblendfactor" "0.5""#), 0.5);
        assert_eq!(
            w(r#""$detail" "d" "$phongalbedoboost" "3""#),
            1.0,
            "the detail blend factor wins and defaults to 1"
        );
    }

    #[test]
    fn the_modulation_control_has_three_states() {
        // `bNoTint ? -1 : ( 1 - $blendtintbybasealpha )`, which the shader
        // reads as `saturate( baseColor.a + control )`: 1 applies all of
        // `$color`, 0 lets base alpha decide, and -1 applies none of it.
        let z = |body: &str| phong_uniforms(&phong_vmt(body)).shader_controls[2];
        assert_eq!(z(""), 1.0);
        assert_eq!(z(r#""$blendtintbybasealpha" "1""#), 0.0);
        assert_eq!(z(r#""$notint" "1""#), -1.0);
        // `$notint` wins outright, which is the order of the C ternary.
        assert_eq!(z(r#""$notint" "1" "$blendtintbybasealpha" "1""#), -1.0);
    }

    #[test]
    fn the_rim_mask_needs_the_exponent_map_as_well_as_the_flag() {
        // The mask lives in the exponent texture's alpha, so `bHasRimMaskMap`
        // requires all three of the map, `$rimlight` and `$rimmask`. **No
        // Portal 2 material sets `$rimmask`**, so this is 0 across the shipped
        // game and the shader's `fRimMask` is 1.
        let x = |body: &str| phong_uniforms(&phong_vmt(body)).rim_params[0];
        assert_eq!(x(r#""$rimlight" "1" "$rimmask" "1""#), 0.0, "no map");
        assert_eq!(
            x(r#""$rimlight" "1" "$rimmask" "1" "$phongexponenttexture" "e""#),
            1.0
        );
        assert_eq!(
            x(r#""$rimmask" "1" "$phongexponenttexture" "e""#),
            0.0,
            "no rim light"
        );
    }

    #[test]
    fn phong_does_not_reconcile_alpha_testing_against_base_alpha() {
        // `InitVertexLitGeneric_DX9` returns into `InitPhong_DX9` at `:361`,
        // well before the *"Don't alpha test if the alpha channel is used for
        // other purposes"* clear at `:419` — so where `VertexLitGeneric` drops
        // `$alphatest` for a `$selfillum` material, `Phong` keeps it. Valve's
        // asymmetry; one of the 317 sets `$alphatest`.
        let body = r#""$alphatest" "1" "$selfillum" "1""#;
        assert_eq!(
            phong_uniforms(&phong_vmt(body)).flags & PhongFlags::ALPHA_TEST,
            PhongFlags::ALPHA_TEST
        );
        assert_eq!(
            vertex_lit_uniforms(&model_vmt(body)).flags & VertexLitFlags::ALPHA_TEST,
            0,
            "the other shader gives it up"
        );
    }

    #[test]
    fn the_phong_envmap_mask_is_base_alpha_unless_the_normal_map_is_asked_for() {
        // `fEnvMapMask = lerp( baseColor.a, fSpecMask, g_bHasNormalMapAlphaEnvmapMask )`
        // (`phong_ps20b.fxc:672`) — **no `$basealphaenvmapmask` test on the
        // false side**, unlike `VertexLitGeneric`, where base-alpha masking
        // has to be requested. So the flag this `w` carries is the only choice
        // the material gets, and `$basealphaenvmapmask` is inert: 18 of the
        // 317 set it and it changes nothing.
        let w = |body: &str| phong_uniforms(&phong_vmt(body)).envmap_tint[3];
        assert_eq!(w(r#""$envmap" "metal/e""#), 0.0, "base alpha by default");
        assert_eq!(
            w(r#""$envmap" "metal/e" "$basealphaenvmapmask" "1""#),
            0.0,
            "which is what the flag would have asked for anyway"
        );
        assert_eq!(
            w(r#""$envmap" "metal/e" "$normalmapalphaenvmapmask" "1""#),
            1.0
        );
        // `$envmapmask` is not even bound, so there is no third source.
        assert!(!texture_requests(ShaderKind::Phong, &phong_vmt(""))
            .iter()
            .any(|request| request.param == "$envmapmask"));
    }

    #[test]
    fn env_cubemap_turns_the_phong_reflection_off_too() {
        // 92 of the 123 Phong materials with an `$envmap` say `env_cubemap`,
        // which names no file — the cubemap would arrive per draw from the
        // render instance, and that lookup is not written. So `CUBEMAP` is off
        // for them and 31 materials reflect.
        let flag = |body: &str| phong_uniforms(&phong_vmt(body)).flags & PhongFlags::ENVMAP;
        assert_eq!(flag(r#""$envmap" "env_cubemap""#), 0);
        assert_eq!(
            flag(r#""$envmap" "metal/black_wall_envmap_002a""#),
            PhongFlags::ENVMAP
        );
    }

    #[test]
    fn phong_binds_three_textures_the_other_model_shader_does_not() {
        let params = |kind: ShaderKind, vmt: &Vmt| {
            texture_requests(kind, vmt)
                .into_iter()
                .map(|request| request.param)
                .collect::<Vec<_>>()
        };
        let phong = params(ShaderKind::Phong, &phong_vmt(""));
        for name in [
            "$phongexponenttexture",
            "$lightwarptexture",
            "$phongwarptexture",
        ] {
            assert!(phong.contains(&name), "Phong binds {name}");
        }
        // And **not** `$envmapmask`: there is no such sampler in
        // `phong_ps20b.fxc`, so the reflection's mask is base alpha or the
        // normal map's alpha. One shipped material sets the parameter and gets
        // nothing.
        assert!(!phong.contains(&"$envmapmask"));

        let lit = params(ShaderKind::VertexLitGeneric, &model_vmt(""));
        assert!(lit.contains(&"$envmapmask"));
        assert!(!lit.contains(&"$phongexponenttexture"));

        // All three of Phong's extras are *data*, not colour: Valve loads each
        // with no `TEXTUREFLAGS_SRGB`. The exponent map is the clearest case —
        // its red channel is an exponent in the range 1..150.
        for request in texture_requests(ShaderKind::Phong, &phong_vmt("")) {
            if request.param.contains("warp") || request.param.contains("exponent") {
                assert_eq!(
                    request.color_space,
                    ColorSpace::Linear,
                    "{} is data",
                    request.param
                );
            }
        }
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
        // `LightmappedGeneric` does not — nor does `Phong`, whose own shadow
        // block has no `MATERIAL_VAR_MULTIPLY` case, so a material that asks
        // for both `$phong` and `$multiply` loses the second. None of the
        // game's 317 Phong materials does.
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
            let state = render_state(kind, &parse(text, "m.vmt"), ResolvedTextures::default());
            assert_eq!(state.blend, BlendMode::Multiply, "{}", kind.name());
        }
        let state = render_state(
            ShaderKind::LightmappedGeneric,
            &parse(&world, "w.vmt"),
            ResolvedTextures::default(),
        );
        assert_eq!(
            state.blend,
            BlendMode::None,
            "a world surface ignores $multiply in Valve's engine too"
        );
        let phong = format!(r#""VertexLitGeneric" {{ "$phong" "1" "$bumpmap" "b" {body} }}"#);
        let state = render_state(
            ShaderKind::Phong,
            &parse(&phong, "p.vmt"),
            ResolvedTextures::default(),
        );
        assert_eq!(
            state.blend,
            BlendMode::None,
            "and so does a phong model, for a different missing case"
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
            ShaderKind::VertexLitGeneric.context_binding(),
            Some(ContextBinding::ModelLighting)
        );
        assert_eq!(
            ShaderKind::LightmappedGeneric.context_binding(),
            Some(ContextBinding::LightmapPage)
        );
        assert_eq!(ShaderKind::UnlitGeneric.context_binding(), None);
    }

    #[test]
    fn refract_is_a_shader_name_and_its_fallback_is_not() {
        assert_eq!(ShaderKind::from_name("refract"), Some(ShaderKind::Refract));
        assert_eq!(ShaderKind::from_name("Refract"), Some(ShaderKind::Refract));
        // `DEFINE_FALLBACK_SHADER( Refract, Refract_DX90 )` — the fallback
        // mechanism is deleted, so the name it selected is not a shader.
        assert_eq!(ShaderKind::from_name("Refract_DX90"), None);
        // Two separate §7.8 entries that are not this one.
        assert_eq!(ShaderKind::from_name("Portal_Refract"), None);
        assert_eq!(ShaderKind::from_name("EyeRefract"), None);
    }

    #[test]
    fn refract_reads_the_frame_buffer_and_binds_no_lighting() {
        let kind = ShaderKind::Refract;
        // Group 3 is the copy of the scene, *not* a lighting shape: this
        // shader has no diffuse term at all.
        assert_eq!(
            kind.context_binding(),
            Some(ContextBinding::FrameBufferCopy)
        );
        assert_eq!(lighting(kind, &refract_vmt("")), Lighting::None);
        assert!(!lighting(kind, &refract_vmt("")).needs_lightmap());
        // A model layout, pinned: see `ShaderKind::vertex_layout`.
        assert_eq!(kind.vertex_layout(), VertexLayout::Model);
    }

    #[test]
    fn a_local_refract_material_needs_no_copy_of_the_frame_buffer() {
        let kind = ShaderKind::Refract;
        // `InitParamsRefract_DX9`: `$localrefract` is the whole test, and it
        // is the one thing that decides which of the engine's two passes a
        // refractor is drawn in.
        assert!(!needs_frame_buffer_copy(
            kind,
            &refract_vmt(CONTAINER_WINDOW_WARM)
        ));
        assert!(needs_frame_buffer_copy(
            kind,
            &refract_vmt(r#""$normalmap" "n" "$envmap" "env_cubemap""#)
        ));
        // Nothing else in the set ever wants one.
        assert!(!needs_frame_buffer_copy(
            ShaderKind::VertexLitGeneric,
            &vmt("")
        ));
        assert!(!needs_frame_buffer_copy(
            ShaderKind::LightmappedGeneric,
            &vmt("")
        ));
    }

    #[test]
    fn a_refract_material_with_a_base_texture_warps_that_instead() {
        let uniforms = refract_uniforms(
            &refract_vmt(CONTAINER_WINDOW_WARM),
            ResolvedTextures::default(),
        );
        assert_ne!(
            uniforms.flags & RefractFlags::BASE_TEXTURE,
            0,
            "the shader must sample group 1, not the frame-buffer copy"
        );
        // Without one, the flag is off and group 3 is the source.
        let uniforms = refract_uniforms(
            &refract_vmt(r#""$normalmap" "n""#),
            ResolvedTextures::default(),
        );
        assert_eq!(uniforms.flags & RefractFlags::BASE_TEXTURE, 0);
    }

    #[test]
    fn bluramount_is_an_integer_so_a_fraction_means_no_blur() {
        let blur = |body: &str| {
            refract_uniforms(&refract_vmt(body), ResolvedTextures::default()).flags
                & RefractFlags::BLUR
                != 0
        };
        // The 16 `props_destruction` glass materials that write ".3", and the
        // two that write ".25", all mean 0 — `GetIntValue()` truncates.
        assert!(!blur(r#""$bluramount" ".3""#));
        assert!(!blur(r#""$bluramount" ".25""#));
        assert!(!blur(r#""$bluramount" ".5""#));
        assert!(!blur(r#""$bluramount" "0""#));
        assert!(!blur(""), "InitParams writes 0 for an undefined one");
        assert!(blur(r#""$bluramount" "1""#));
        // `MAXBLUR` is 1, so the `BLUR > 1` branch of the pixel shader is
        // unreachable and 2 is the same pipeline as 1.
        assert!(blur(r#""$bluramount" "2""#));
    }

    #[test]
    fn the_local_refract_aspect_fixup_is_integer_division() {
        // `float( nHeight / nWidth )` with both operands `int`
        // (`refract_dx9_helper.cpp:283`). `glass/refract_light_color` is
        // 128x512 in the shipped game, so five glass materials get 4 — not
        // 0.25, and not 4.0-by-accident.
        let fixup = |width: u32, height: u32| {
            refract_uniforms(
                &refract_vmt(CONTAINER_WINDOW_WARM),
                ResolvedTextures {
                    base: Some(TextureFacts {
                        width,
                        height,
                        translucent: true,
                    }),
                    normal_map: None,
                },
            )
            .refract_params[2]
        };
        assert_eq!(fixup(128, 512), 4.0, "glass/refract_light_color");
        assert_eq!(fixup(128, 128), 1.0, "glass/refract_light_color_container");
        // The case that loses the horizontal offset entirely. No shipped
        // `$localrefract` material is wider than it is tall; this pins the
        // behaviour so nobody "fixes" the cast.
        assert_eq!(fixup(512, 128), 0.0, "a wider-than-tall source truncates");
    }

    #[test]
    fn a_colour_parameter_is_gamma_decoded_the_way_the_shader_helper_decodes_it() {
        // `val > 1 ? val : GammaToLinear( val )`, and `GammaToLinear` is a
        // 256-entry table that returns 1 for anything at or above 0.95.
        let decoded = gamma_to_linear_param([0.4, 0.96, 1.5, 0.25]);
        assert!((decoded[0] - 0.4f32.powf(2.2)).abs() < 1e-4, "{decoded:?}");
        assert_eq!(decoded[1], 1.0, "0.95 and up clamps to white");
        assert_eq!(decoded[2], 1.5, "above 1 passes through, un-decoded");
        assert_eq!(decoded[3], 0.25, "w is left alone");
        assert_eq!(gamma_to_linear_param([-1.0; 4])[0], 0.0);

        // `$refracttint "{235 247 247}"`, which sixteen shipped materials
        // write: braces divide by 255, and then two of the three channels are
        // over the clamp.
        let uniforms = refract_uniforms(
            &refract_vmt(r#""$refracttint" "{235 247 247}""#),
            ResolvedTextures::default(),
        );
        assert!((uniforms.refract_tint[0] - (235.0f32 / 255.0).powf(2.2)).abs() < 1e-4);
        assert_eq!(uniforms.refract_tint[1], 1.0);
        assert_eq!(uniforms.refract_tint[2], 1.0);
    }

    /// The *other* decode — `GammaToLinearFullRange`, which is what
    /// `VertexLitGeneric`'s `$envmaptint` takes, and the two are not
    /// interchangeable.
    #[test]
    fn the_envmap_tint_takes_the_full_range_decode_and_not_the_table_one() {
        let full = gamma_to_linear_full_range_param([0.4, 0.96, 5.0, 0.25]);
        let table = gamma_to_linear_param([0.4, 0.96, 5.0, 0.25]);

        // They agree below the table's clamp, because both are `pow( x, 2.2 )`
        // there and 0.4 survives the quantization to `round( x * 255 ) / 255`.
        assert!((full[0] - 0.4f32.powf(2.2)).abs() < 1e-6, "{full:?}");
        assert!((full[0] - table[0]).abs() < 1e-4);

        // And disagree at both ends. `>= 0.95` is white under the table and an
        // ordinary decode here...
        assert_eq!(table[1], 1.0);
        assert!((full[1] - 0.96f32.powf(2.2)).abs() < 1e-6, "{full:?}");
        assert!(full[1] < 0.92, "not clamped to white: {}", full[1]);

        // ...and `> 1` passes through the table un-decoded where this really
        // does raise it to the power, which is `(wills)`'s over-driven tint.
        // `models/sabotage/glass01` writes `[5 5 5]`, the one material in
        // Portal 2 that tells the two functions apart.
        assert_eq!(table[2], 5.0);
        assert!((full[2] - 34.4932).abs() < 1e-3, "{full:?}");

        // `w` is `$envmapcontrast` in this port's packing, and neither
        // function touches it.
        assert_eq!(full[3], 0.25);

        // No domain guard, the same as C's `pow` — so a negative component is
        // a NaN rather than the table's 0. No shipped material reaches it.
        assert!(gamma_to_linear_full_range_param([-1.0; 4])[0].is_nan());
        assert_eq!(gamma_to_linear_param([-1.0; 4])[0], 0.0);
    }

    #[test]
    fn a_vertex_lit_envmap_tint_reaches_the_shader_linear() {
        // `[0.05 0.05 0.05]` is what twenty of the game's 57 reflective
        // `VertexLitGeneric` materials write, and it is a factor of 36 darker
        // decoded than not — which is the whole visible effect of this.
        let uniforms = vertex_lit_uniforms(&model_vmt(
            r#""$envmap" "metal/x" "$envmaptint" "[.05 .05 .05]""#,
        ));
        let expected = 0.05f32.powf(2.2);
        assert!(
            (uniforms.envmap_tint[0] - expected).abs() < 1e-6,
            "{uniforms:?}"
        );
        assert!((expected - 0.0013732).abs() < 1e-6);
        assert_eq!(uniforms.envmap_tint[..3], [uniforms.envmap_tint[0]; 3]);

        // `$envmapcontrast` shares the register and is not a colour.
        let uniforms =
            vertex_lit_uniforms(&model_vmt(r#""$envmap" "metal/x" "$envmapcontrast" ".5""#));
        assert_eq!(uniforms.envmap_tint[3], 0.5, "not decoded");
        // An undefined tint is `SetVecValue( 1, 1, 1 )`, and white decodes to
        // white, so every material that leaves it alone is unaffected.
        assert_eq!(uniforms.envmap_tint[..3], [1.0, 1.0, 1.0]);

        // The neighbouring tints in the same block deliberately do *not*
        // decode: Valve hands both to the shader as written.
        let uniforms = vertex_lit_uniforms(&model_vmt(
            r#""$selfillumtint" "[.5 .5 .5]" "$detailtint" "[.5 .5 .5]""#,
        ));
        assert_eq!(uniforms.selfillum_tint[..3], [0.5, 0.5, 0.5]);
        assert_eq!(uniforms.detail_tint[..3], [0.5, 0.5, 0.5]);
    }

    #[test]
    fn refract_takes_its_defaults_from_init_params_and_not_from_the_type() {
        // `InitParamsRefract_DX9` writes real values for four of these, and
        // `$localrefractdepth`'s differs from the declared default of 0.
        let uniforms = refract_uniforms(&refract_vmt(""), ResolvedTextures::default());
        assert_eq!(uniforms.envmap_tint[..3], [1.0, 1.0, 1.0], "white");
        assert_eq!(uniforms.envmap_tint[3], 0.0, "$envmapcontrast");
        assert_eq!(uniforms.refract_params[1], 1.0, "$envmapsaturation");
        assert_eq!(uniforms.refract_params[3], 0.05, "$localrefractdepth");
        // And one that has no `SHADER_INIT_PARAMS` default, so 0 is right.
        assert_eq!(uniforms.refract_params[0], 0.0, "$refractamount");
    }

    #[test]
    fn refract_blends_from_its_normal_map_and_only_without_an_envmap() {
        let normal = |translucent| {
            Some(TextureFacts {
                width: 128,
                height: 512,
                translucent,
            })
        };

        // `SetDefaultBlendingShadowState( m_nNormalMap, false )` is guarded by
        // `!bHasEnvmap`. Every `$model 1` material in the game has an envmap,
        // so every piece of glass in Portal 2 draws with no blending at all.
        let state = render_state(
            ShaderKind::Refract,
            &refract_vmt(r#""$normalmap" "n" "$envmap" "metal/foo""#),
            ResolvedTextures {
                base: None,
                normal_map: normal(true),
            },
        );
        assert_eq!(
            state.blend,
            BlendMode::None,
            "an envmap suppresses the blending decision entirely"
        );
        assert!(state.depth_write);

        // Without one, a normal map with an alpha channel blends.
        let state = render_state(
            ShaderKind::Refract,
            &refract_vmt(r#""$normalmap" "n""#),
            ResolvedTextures {
                base: None,
                normal_map: normal(true),
            },
        );
        assert_eq!(state.blend, BlendMode::Blend);
        assert!(
            !state.depth_write,
            "EnableAlphaBlending turns these off too"
        );
        assert!(!state.write_alpha, "a translucent normal map is not opaque");

        // And one without an alpha channel does not — which is the shipped
        // case: `glass/refract_light_normal` is DXT1.
        let state = render_state(
            ShaderKind::Refract,
            &refract_vmt(r#""$normalmap" "n""#),
            ResolvedTextures {
                base: None,
                normal_map: normal(false),
            },
        );
        assert_eq!(state.blend, BlendMode::None);
        assert!(state.write_alpha);
    }

    #[test]
    fn refract_writes_z_unless_the_material_says_not_to() {
        let state = |body: &str| {
            render_state(
                ShaderKind::Refract,
                &refract_vmt(body),
                ResolvedTextures::default(),
            )
        };
        assert!(state(r#""$normalmap" "n""#).depth_write);
        assert!(!state(r#""$normalmap" "n" "$nowritez" "1""#).depth_write);
        // `SetInitialShadowState` still applies to this shader.
        assert!(!state(r#""$nocull" "1""#).cull);
        assert!(!state(r#""$ignorez" "1""#).depth_test);
    }

    #[test]
    fn refract_declares_the_textures_it_samples_and_no_others() {
        let requests = texture_requests(ShaderKind::Refract, &refract_vmt(""));
        let by_param = |param: &str| requests.iter().find(|r| r.param == param).copied();

        // The image to warp is sRGB colour; the normal map is data.
        assert_eq!(
            by_param("$basetexture").map(|r| r.color_space),
            Some(ColorSpace::Srgb)
        );
        assert_eq!(
            by_param("$normalmap").map(|r| r.color_space),
            Some(ColorSpace::Linear)
        );
        // The refract tint texture is multiplied into the image, so it is
        // colour — unlike every other mask-shaped texture in the set.
        assert_eq!(
            by_param("$refracttinttexture").map(|r| r.color_space),
            Some(ColorSpace::Srgb)
        );
        // **sRGB, unconditionally**, where `VertexLitGeneric`'s is linear
        // because Portal 2 ships HDR. Valve's asymmetry:
        // `LoadCubeMap( m_nEnvmap, TEXTUREFLAGS_SRGB | ANISOTROPIC_OVERRIDE )`
        // against `GetHDRType() == HDR_TYPE_NONE ? TEXTURE_FLAGS_SRGB : 0`.
        let envmap = by_param("$envmap").expect("an envmap request");
        assert_eq!(envmap.color_space, ColorSpace::Srgb);
        assert_eq!(envmap.dimension, TextureDimension::Cube);
        let vertex_lit = texture_requests(ShaderKind::VertexLitGeneric, &vmt(""));
        assert_eq!(
            vertex_lit
                .iter()
                .find(|r| r.param == "$envmap")
                .map(|r| r.color_space),
            Some(ColorSpace::Linear)
        );

        // `$normalmap` shares the bump binding, because that is what it is.
        assert_eq!(
            by_param("$normalmap").map(|r| r.binding),
            Some(BINDING_BUMP_TEXTURE)
        );
        // Nothing this shader does not read.
        assert!(by_param("$bumpmap").is_none());
        assert!(by_param("$detail").is_none());
        assert!(by_param("$envmapmask").is_none());
    }

    #[test]
    fn the_refract_parameter_table_promises_only_what_is_implemented() {
        let kind = ShaderKind::Refract;
        for name in [
            "$refractamount",
            "$refracttint",
            "$normalmap",
            "$bumptransform",
            "$bluramount",
            "$fadeoutonsilhouette",
            "$envmap",
            "$envmaptint",
            "$envmapcontrast",
            "$envmapsaturation",
            "$refracttinttexture",
            "$nowritez",
            "$localrefract",
            "$localrefractdepth",
            // From `STANDARD_PARAMS`, and `$basetexture` is load-bearing here.
            "$basetexture",
            "$alpha",
        ] {
            assert!(kind.param(name).is_some(), "{name}");
        }
        // Dead in Valve's own shader, or pinned on a content measurement —
        // see `REFRACT_PARAMS`. A table entry is a promise.
        for name in [
            "$time",
            "$fresnelreflection",
            "$normalmap2",
            "$bumptransform2",
            "$masked",
            "$magnifyenable",
            "$magnifyscale",
            "$noviewportfixup",
            "$mirroraboutviewportedges",
            "$vertexcolormodulate",
        ] {
            assert!(kind.param(name).is_none(), "{name}");
        }
    }

    #[test]
    fn the_refract_uniform_block_is_the_size_wgsl_expects() {
        // Two transform rows, three vectors, and a flag word padded out by
        // hand — WGSL rounds the struct up to 16 and Rust does not.
        assert_eq!(size_of::<RefractUniforms>(), 2 * 16 + 3 * 16 + 16);
        assert_eq!(size_of::<RefractUniforms>() % 16, 0);
    }
}
