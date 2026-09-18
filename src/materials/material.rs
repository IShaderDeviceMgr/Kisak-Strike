//! Materials: a `.vmt` bound to a shader, its textures and its pipeline state.
//!
//! Replaces `materialsystem/cmaterial.cpp`'s second half — `InitializeShader`,
//! `Precache_Internal`, `RecomputeStateSnapshots` — and
//! `materialsystem/cmaterialdict.cpp`, plus the material-facing part of
//! `cmaterialsystem.cpp` (`FindMaterial`, `CreateMaterial`,
//! `CreateDebugMaterials`).
//!
//! What is *not* here, and why:
//!
//! | In the original | Here |
//! |---|---|
//! | `Precache`/`Uncache`/`Refresh`, `MATERIAL_IS_PRECACHED` | construction is precaching; a [`Material`] is always ready to draw |
//! | `m_RefCount`, `IncrementReferenceCount`, `DeleteIfUnreferenced` | `Arc` |
//! | `GetFallbackShader` and the `$fallbackmaterial` loop | deleted with the hardware variety that motivated it (§4.1) |
//! | `IMaterialProxy` and `InitializeMaterialProxy` | deferred; the concept survives as a per-frame hook over the vars, the `CreateInterface` factory does not |
//! | `CMaterialSubRect` | not ported — §10 asks whether anything outside the module creates one |
//! | Queue-friendly duplicates (`m_QueueFriendlyVersion`) | deleted with the queued context (§5.3) |
//!
//! # A material is built once and never changes
//!
//! Everything a `.vmt` decides — the shader, the textures, the pipeline state,
//! the uniform block, the bind group — is resolved in [`Material::new`] and
//! then immutable. The original recomputed state snapshots whenever a var
//! changed, because proxies and `IMaterial::AlphaModulate` could change them at
//! any time. When proxies land, the mutable part is the *draw* uniforms, not
//! this.

use std::collections::HashMap;
use std::sync::Arc;

use crate::filesystem::{keyvalues, Vfs};

use super::error::VmtError;
use super::pipeline::{BindLayouts, BlendMode, PipelineCache, RenderState};
use super::shader::{self, Lighting, ResolvedTextures, ShaderKind, TextureDimension, TextureFacts};
use super::texture::{Texture, TextureCache};
use super::var::MaterialFlags;
use super::vmt::Vmt;

/// A material: everything needed to draw with it, resolved.
pub struct Material {
    /// The name it was found under — lowercased, no extension.
    pub name: String,
    pub shader: ShaderKind,
    pub flags: MaterialFlags,
    /// The pipeline state its flags asked for. Half of a [`PipelineKey`]; the
    /// other half is the target, which the frame supplies.
    ///
    /// **This is the snapshot for a draw that is not alpha-modulating.** Use
    /// [`state_for`](Material::state_for) rather than this field directly
    /// unless the modulation is known to be 1.
    pub state: RenderState,
    /// The same shadow phase re-run with `SHADER_USING_ALPHA_MODULATION` set.
    ///
    /// Valve's materials carry up to eight `StateSnapshot_t`s, one per
    /// combination of modulation flags, and `CShaderAPIDx8::DrawMesh2`
    /// (`shaderapidx8.cpp:4907`) picks one from the instance's diffuse
    /// modulation (`:4944`). Only the alpha
    /// bit is reachable in this port — the other three are the flashlight, the
    /// editor and paint — so there are two, and
    /// [`state_for`](Material::state_for) is the pick.
    pub state_alpha_modulated: RenderState,
    /// `$color * $color2` with `$alpha` in `w`, ready for
    /// [`DrawUniforms::modulation`](super::uniforms::DrawUniforms::modulation).
    ///
    /// Lives on the material but belongs to the draw: the render context
    /// multiplies it by a per-instance modulation before it reaches the GPU,
    /// which is why it is not baked into the material's own uniform block.
    pub modulation: [f32; 4],

    /// How this material is lit, and therefore how wide a lightmap block its
    /// surfaces reserve in the atlas.
    ///
    /// `IMaterial::GetPropertyFlag( MATERIAL_PROPERTY_NEEDS_LIGHTMAP )` and
    /// `..._NEEDS_BUMPED_LIGHTMAPS` (`cmaterial.cpp:2946`) as one answer,
    /// because the two are never asked separately: `RegisterLightmappedSurface`
    /// (`gl_matsysiface.cpp:216`) asks the second only after the first said
    /// yes. See [`Lighting`].
    pub lighting: Lighting,

    /// Whether a model wearing this material is lit per pixel, and so reads
    /// no baked per-vertex lighting.
    ///
    /// `STUDIOHDR_FLAGS_USES_BUMPMAPPING`, which a model gets if *any* of its
    /// materials sets it. Read by
    /// [`PropModels`](crate::engine::world::props::PropModels), which is where
    /// the consequence is — see [`shader::uses_bumpmapping`].
    pub uses_bumpmapping: bool,

    /// Whether drawing this material needs a readable copy of the scene so
    /// far, taken before the draw.
    ///
    /// `MATERIAL_VAR2_NEEDS_POWER_OF_TWO_FRAME_BUFFER_TEXTURE`, which reaches
    /// the renderer as `ERENDERFLAGS_NEEDS_POWER_OF_TWO_FB` and is what makes
    /// `CRendering3dView::DrawTranslucentRenderables` call
    /// `UpdateRefractTexture` (`game/client/viewrender.cpp:6195`). Here it is
    /// what sorts a static prop's batches into the two passes
    /// [`World::draw`](crate::engine::world::World::draw) and
    /// [`World::draw_refracting`](crate::engine::world::World::draw_refracting)
    /// record. See [`shader::needs_frame_buffer_copy`].
    pub needs_frame_buffer_copy: bool,

    /// Kept so the views the bind group holds stay alive.
    #[allow(dead_code)]
    textures: Vec<Arc<Texture>>,
    #[allow(dead_code)]
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Material {
    /// Binds the material's textures and parameters — bind group 1.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Which of the two snapshots a draw with this much modulation alpha uses.
    ///
    /// `bIsAlphaModulating = pInstances[0].m_DiffuseModulation[3] != 1.0f`
    /// (`shaderapidx8.cpp:4944`) — an **exact** comparison against one, on the
    /// product of the material's own modulation and the instance's, which is
    /// what [`Pass::draw_modulated`] passes in.
    ///
    /// [`Pass::draw_modulated`]: super::context::Pass::draw_modulated
    pub fn state_for(&self, modulation_alpha: f32) -> RenderState {
        match modulation_alpha != 1.0 {
            true => self.state_alpha_modulated,
            false => self.state,
        }
    }

    /// Whether this belongs in the translucent pass — `CMaterial::IsTranslucent`
    /// (`cmaterial.cpp:2979`).
    ///
    /// Valve ORs four things. Three of them have a counterpart here, and the
    /// third is what makes this more than `blend != None`:
    ///
    /// - `::IsTranslucent( &m_ShaderRenderState )`, which is
    ///   `m_AlphaBlendEnable && !m_AlphaBlendEnabledForceOpaque`
    ///   (`shaderapidx8.cpp:4005`) — here [`RenderState::blend`], since
    ///   `EnableBlendingForceOpaque` is `water.cpp`'s alone and water is not
    ///   ported.
    /// - `fAlphaModulation < 1.0f`, which is already folded into the blend
    ///   above because [`render_state`](super::shader::render_state) reads
    ///   `$alpha`. An *instance* alpha below 1 is not a property of the
    ///   material and is the caller's to notice — see
    ///   [`state_for`](Material::state_for).
    /// - `m_pShader->IsTranslucent( params )`, which for every shader in the
    ///   target set is `CBaseShader`'s `IS_FLAG_SET( MATERIAL_VAR_TRANSLUCENT )`
    ///   (`BaseShader.cpp:723`). **This is not implied by the blend**:
    ///   `TextureIsTranslucent` only says yes when the base texture really has
    ///   an alpha channel, so `$translucent 1` over an opaque texture is
    ///   classified translucent and yet draws with blending off. Valve sorts it
    ///   into the translucent list all the same, and so does this.
    /// - `MATERIAL_VAR_ALPHA_MODIFIED_BY_PROXY`, which no material can set
    ///   here: proxies are unported.
    pub fn is_translucent(&self) -> bool {
        self.state.blend != BlendMode::None || self.flags.contains(MaterialFlags::TRANSLUCENT)
    }

    /// Resolves a parsed `.vmt` into something drawable.
    ///
    /// `resolve` turns a texture name into a texture. It is a callback rather
    /// than a `&mut TextureCache` because the two callers differ in what they
    /// can do: an ordinary material reads through the [`Vfs`], and the error
    /// material — which has to exist before any content is mounted — hands back
    /// the checkerboard without looking anything up. That is
    /// `CMaterialSystem::CreateDebugMaterials` (`cmaterialsystem.cpp:462`)
    /// building `___error.vmt` in memory at startup, which is exactly the same
    /// bootstrap problem.
    ///
    /// The `.vmt` is the source of every decision here, in this order:
    /// the shader name picks the code, the texture params pick the textures
    /// (and their colour space), the resolved textures and the flags pick the
    /// pipeline state, and the vars fill the uniform block. That order is
    /// forced: [`render_state`](super::shader::render_state) cannot decide
    /// blending without knowing whether the base texture has an alpha channel.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layouts: &BindLayouts,
        name: &str,
        vmt: &Vmt,
        fallback: &TextureFallbacks,
        mut resolve: impl FnMut(&str, super::ColorSpace, TextureDimension) -> Arc<Texture>,
    ) -> Option<Material> {
        // **`resolve`, not `from_name`.** A `.vmt` that names
        // `VertexLitGeneric` and asks for `$phong` is drawn by `Phong`, which
        // is a different WGSL module, a different bind group layout and a
        // different parameter table — see `ShaderKind::resolve`.
        let shader = ShaderKind::resolve(vmt)?;

        let mut textures = Vec::new();
        let mut entries = Vec::new();
        for request in shader::texture_requests(shader, vmt) {
            let texture = match texture_name(vmt, request.param) {
                Some(texture_name) => resolve(texture_name, request.color_space, request.dimension),
                // An *undefined* texture parameter is not a failure and must
                // not draw as one: a `.vmt` with no `$basetexture` is a legal
                // material whose colour comes from `$color` and the vertex
                // stream, and every shader binds the standard white texture for
                // it (`vertexlitgeneric_dx9_helper.cpp:1255`). Valve's own
                // `___flat.vmt` is one.
                None => Arc::clone(fallback.unset(request.dimension)),
            };
            // A bind group layout names a view dimension, so binding a cubemap
            // where the shader declared a 2D texture — or the reverse — is a
            // `wgpu` validation error rather than a wrong picture. It gets the
            // same treatment a broken texture does, in the shape the layout
            // wants.
            let texture = if texture.view_dimension == request.dimension.view_dimension() {
                texture
            } else {
                eprintln!(
                    "source-engine: materials: {name}: {} is a {:?} texture, which {} samples as {:?}",
                    request.param,
                    texture.view_dimension,
                    shader.name(),
                    request.dimension.view_dimension(),
                );
                Arc::clone(fallback.broken(request.dimension))
            };
            textures.push((request, texture));
        }

        // Resolved before the uniform block, because `Refract` needs the base
        // texture's *dimensions* to fill it and every shader needs the shadow
        // phase's answers below.
        let resolved = ResolvedTextures {
            base: find_texture(&textures, "$basetexture"),
            normal_map: find_texture(&textures, "$normalmap"),
        };

        let uniforms = match shader {
            ShaderKind::UnlitGeneric => {
                let block = shader::unlit_uniforms(vmt);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
            ShaderKind::LightmappedGeneric | ShaderKind::WorldVertexTransition => {
                let block = shader::lightmapped_uniforms(shader, vmt);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
            ShaderKind::VertexLitGeneric => {
                let block = shader::vertex_lit_uniforms(vmt);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
            ShaderKind::Phong => {
                let block = shader::phong_uniforms(vmt);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
            ShaderKind::Refract => {
                let block = shader::refract_uniforms(vmt, resolved);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
        };
        entries.push(wgpu::BindGroupEntry {
            binding: shader::BINDING_MATERIAL_UNIFORMS,
            resource: uniforms.as_entire_binding(),
        });
        for (request, texture) in &textures {
            entries.push(wgpu::BindGroupEntry {
                binding: request.binding,
                resource: wgpu::BindingResource::TextureView(texture.view()),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: request.binding + 1,
                resource: wgpu::BindingResource::Sampler(texture.sampler()),
            });
        }

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(name),
            layout: layouts.material(shader),
            entries: &entries,
        });

        Some(Material {
            name: name.to_owned(),
            shader,
            flags: vmt.flags,
            state: shader::render_state(shader, vmt, resolved),
            state_alpha_modulated: shader::render_state_modulated(shader, vmt, resolved),
            modulation: shader::modulation_color(shader, vmt),
            lighting: shader::lighting(shader, vmt),
            uses_bumpmapping: shader::uses_bumpmapping(vmt),
            needs_frame_buffer_copy: shader::needs_frame_buffer_copy(shader, vmt),
            textures: textures.into_iter().map(|(_, texture)| texture).collect(),
            uniforms,
            bind_group,
        })
    }
}

/// The texture a parameter resolved to, for the shadow phase.
///
/// `None` when the shader did not ask for that parameter at all — a
/// `LightmappedGeneric` material has no `$normalmap` request — which reads as
/// "opaque", the same answer the white texture and the checkerboard give.
fn find_texture(
    textures: &[(shader::TextureRequest, Arc<Texture>)],
    param: &str,
) -> Option<TextureFacts> {
    textures
        .iter()
        .find(|(request, _)| request.param == param)
        .map(|(_, texture)| TextureFacts::of(texture))
}

/// The two standard textures [`Material::new`] substitutes, and the reason they
/// are two: `CTextureManager` keeps a whole family of them
/// (`texturemanager.cpp:640-685`) and the shaders pick between them by
/// *situation*, not by failure. A parameter nobody set gets `white`; one that
/// was set and could not be honoured gets the checkerboard.
pub struct TextureFallbacks {
    pub white: Arc<Texture>,
    pub error: Arc<Texture>,
    /// The cube-shaped substitute for both cases. There is no "error cubemap"
    /// in the original and there is no useful one to invent: a checkerboard
    /// reflection reads as a broken *world*, not a broken material, so an
    /// unresolvable `$envmap` gets the same black cube an unset one does and
    /// the failure is reported on stderr instead. See
    /// [`Texture::black_cube`](super::texture::Texture::black_cube).
    pub black_cube: Arc<Texture>,
}

impl TextureFallbacks {
    /// What a parameter nobody set binds, in the shape the layout wants.
    fn unset(&self, dimension: TextureDimension) -> &Arc<Texture> {
        match dimension {
            TextureDimension::D2 => &self.white,
            TextureDimension::Cube => &self.black_cube,
        }
    }

    /// What a parameter that was set and could not be honoured binds.
    fn broken(&self, dimension: TextureDimension) -> &Arc<Texture> {
        match dimension {
            TextureDimension::D2 => &self.error,
            TextureDimension::Cube => &self.black_cube,
        }
    }
}

/// The texture name a parameter resolves to, if it names one that can be
/// loaded.
///
/// One special case, and it is not a quirk of this port:
/// **`$envmap "env_cubemap"` names no file.** `CShaderSystem::LoadCubeMap`
/// (`shadersystem.cpp:1840`) sets the var to `(ITexture *)-1` and flags the
/// material `MATERIAL_VAR2_USES_ENV_CUBEMAP`; the cubemap then arrives per
/// draw from the render instance, because which one a model reflects depends
/// on where it is standing. See
/// [`envmap_name`](super::shader::envmap_name) for the full note and the
/// condition that makes it real render-context state.
fn texture_name<'a>(vmt: &'a Vmt, param: &str) -> Option<&'a str> {
    if param.eq_ignore_ascii_case("$envmap") {
        return shader::envmap_name(vmt);
    }
    vmt.var(param).and_then(|var| var.as_str())
}

fn create_uniform_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    bytes: &[u8],
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytes);
    buffer
}

/// The material dictionary, and everything it needs to fill itself.
///
/// `CMaterialDict` plus the parts of `CMaterialSystem` that own the texture
/// manager and the shader system. One object rather than three because the
/// dependency is one-directional and total: building a material needs textures,
/// needs pipeline layouts, and needs neither of them to be swappable.
///
/// There is no refcounting, no `Uncache`, and no eviction — same reasoning as
/// [`TextureCache`], and the same condition for revisiting it: a map to measure
/// against.
pub struct MaterialCache {
    device: wgpu::Device,
    queue: wgpu::Queue,
    textures: TextureCache,
    pipelines: PipelineCache,
    materials: HashMap<String, Arc<Material>>,
    error: Arc<Material>,
    error_model: Arc<Material>,
}

impl MaterialCache {
    /// Builds the cache, the texture cache under it, and the error material.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> MaterialCache {
        let textures = TextureCache::new(device, queue);
        let pipelines = PipelineCache::new(device);

        let fallback = TextureFallbacks {
            white: textures.white_texture(),
            error: textures.error_texture(),
            black_cube: textures.black_cube_texture(),
        };
        let document = keyvalues::parse(ERROR_MATERIAL_NAME, ERROR_MATERIAL)
            .expect("the error material is a literal in this file");
        let vmt = Vmt::from_keyvalues(ERROR_MATERIAL_NAME, &document)
            .expect("the error material names a shader");
        // Its `$basetexture` resolves to the checkerboard without a lookup:
        // this is the material a lookup failure lands on, so it cannot depend
        // on one succeeding.
        let error = Material::new(
            device,
            queue,
            pipelines.layouts(),
            ERROR_MATERIAL_NAME,
            &vmt,
            &fallback,
            |_, _, _| Arc::clone(&fallback.error),
        )
        .expect("the error material's shader is ported");

        let document = keyvalues::parse(ERROR_MODEL_MATERIAL_NAME, ERROR_MODEL_MATERIAL)
            .expect("the error material is a literal in this file");
        let vmt = Vmt::from_keyvalues(ERROR_MODEL_MATERIAL_NAME, &document)
            .expect("the error material names a shader");
        let error_model = Material::new(
            device,
            queue,
            pipelines.layouts(),
            ERROR_MODEL_MATERIAL_NAME,
            &vmt,
            &fallback,
            |_, _, _| Arc::clone(&fallback.error),
        )
        .expect("the error material's shader is ported");

        MaterialCache {
            device: device.clone(),
            queue: queue.clone(),
            textures,
            pipelines,
            materials: HashMap::new(),
            error: Arc::new(error),
            error_model: Arc::new(error_model),
        }
    }

    /// The queue the cache uploads through.
    ///
    /// Exposed for the lightmap atlas, which is built by `src/engine/world/`
    /// against this cache's bind group layouts and so has to upload through
    /// the same device. It is a cheap refcounted handle, not ownership.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The bind group layouts every pipeline is built against.
    ///
    /// Immutable, unlike [`pipelines`](MaterialCache::pipelines), so that a
    /// caller can hold it alongside [`queue`](MaterialCache::queue) — which the
    /// lightmap atlas needs, since building a page's bind group takes both.
    pub fn layouts(&self) -> &BindLayouts {
        self.pipelines.layouts()
    }

    /// The pipeline cache. A caller needs it at draw time to turn a material's
    /// state into something `set_pipeline` accepts.
    pub fn pipelines(&mut self) -> &mut PipelineCache {
        &mut self.pipelines
    }

    /// The magenta checkerboard material, `___error.vmt`.
    pub fn error_material(&self) -> Arc<Material> {
        Arc::clone(&self.error)
    }

    /// The error material in the shape a *model* can be drawn with.
    ///
    /// The checkerboard is one material in the original because a `.vmt`'s
    /// `$model 1` was a parameter and not a different shader: Valve's
    /// `CreateDebugMaterials` writes exactly one error material and sets
    /// `$model` on it (`cmaterialsystem.cpp:465`), and the `UnlitGeneric`
    /// helper picks the vertex format from that flag at draw time.
    ///
    /// This port decides the vertex layout per *shader* instead — that is what
    /// makes [`Pass::draw`](super::context::Pass::draw)'s layout assertion
    /// possible at all — so the same material cannot serve both. The two are
    /// the same `.vmt` under two shader names, and a caller picks the one its
    /// geometry has: brush faces take [`error_material`](Self::error_material),
    /// studio models take this.
    pub fn error_model_material(&self) -> Arc<Material> {
        Arc::clone(&self.error_model)
    }

    /// Loads `materials/<name>.vmt`, or returns the error material.
    ///
    /// **Cannot fail**, for the same reason [`TextureCache::load`] cannot:
    /// `CMaterialSystem::FindMaterial` (`cmaterialsystem.cpp:3032`) answers
    /// every failure — missing file, malformed keyvalues, unknown shader, a
    /// patch chain that does not resolve — with `g_pErrorMaterial`, and a map
    /// with one bad material has to load anyway. The reason is on stderr,
    /// once per name.
    ///
    /// `name` is normalized the way `FindMaterial` normalizes it: lowercased,
    /// forward slashes, extension stripped. So `Metal\Wall01.vmt` and
    /// `metal/wall01` are one entry.
    pub fn load(&mut self, vfs: &Vfs, name: &str) -> Arc<Material> {
        let key = normalize_name(name);
        if let Some(material) = self.materials.get(&key) {
            return Arc::clone(material);
        }

        let material = match self.build(vfs, &key) {
            Ok(material) => Arc::new(material),
            Err(err) => {
                eprintln!("source-engine: materials: {err}");
                Arc::clone(&self.error)
            }
        };
        self.materials.insert(key, Arc::clone(&material));
        material
    }

    /// Reads and resolves one `.vmt`. `name` is already normalized.
    fn build(&mut self, vfs: &Vfs, name: &str) -> Result<Material, VmtError> {
        let vmt = Vmt::load(vfs, name)?;

        // Field-by-field, so the texture cache can be borrowed mutably by the
        // closure while the pipeline layouts are borrowed immutably.
        let MaterialCache {
            device,
            queue,
            textures,
            pipelines,
            ..
        } = self;
        let fallback = TextureFallbacks {
            white: textures.white_texture(),
            error: textures.error_texture(),
            black_cube: textures.black_cube_texture(),
        };

        Material::new(
            device,
            queue,
            pipelines.layouts(),
            name,
            &vmt,
            &fallback,
            |texture_name, color_space, dimension| match dimension {
                // A cubemap goes through the HDR-name rule; a 2D texture has
                // none. See `TextureCache::load_cubemap`.
                TextureDimension::Cube => textures.load_cubemap(vfs, texture_name, color_space),
                TextureDimension::D2 => textures.load(vfs, texture_name, color_space),
            },
        )
        .ok_or_else(|| VmtError::UnknownShader {
            name: name.to_owned(),
            shader: vmt.shader.clone(),
        })
    }
}

/// The name the error material is registered under. `___error.vmt`
/// (`cmaterialsystem.cpp:472`); the leading underscores are Valve's way of
/// keeping it out of the way of content.
const ERROR_MATERIAL_NAME: &str = "___error";

/// The error material, written out as the `KeyValues` document
/// `CreateDebugMaterials` builds in code (`cmaterialsystem.cpp:465-471`).
///
/// **It is an ordinary `UnlitGeneric` with the error checkerboard as its base
/// texture** — the material fallback and the texture fallback are the same
/// mechanism, one layer apart, and that is worth preserving exactly.
///
/// Two of Valve's five keys are dropped: `$decalscale`, which belongs to the
/// decal path, and `$linearwrite`, which disabled the sRGB *write* that this
/// port does not do in the shader anyway (the swap chain's format does it).
/// `$gammacolorread` is kept even though it changes nothing here — the
/// checkerboard is built as an sRGB texture and read as one, so the round trip
/// is the identity either way — because it is the reason the original's
/// checkerboard shows its authored colours, and dropping it would leave the
/// next reader wondering.
const ERROR_MATERIAL: &str = r#"
"UnlitGeneric"
{
	"$basetexture"    "error"
	"$model"          "1"
	"$gammacolorread" "1"
}
"#;

const ERROR_MODEL_MATERIAL_NAME: &str = "___error_model";

/// [`ERROR_MATERIAL`] under the shader that takes model vertices.
///
/// Not a second material in the original — see
/// [`MaterialCache::error_model_material`] for why it has to be one here. It is
/// deliberately the same three keys, so that the checkerboard a broken prop
/// shows is the checkerboard a broken brush face shows.
const ERROR_MODEL_MATERIAL: &str = r#"
"VertexLitGeneric"
{
	"$basetexture"    "error"
	"$model"          "1"
	"$gammacolorread" "1"
}
"#;

/// The dictionary key for a material name.
///
/// `CMaterialSystem::FindMaterial` (`cmaterialsystem.cpp:3045`) lowercases,
/// forward-slashes and strips the extension before looking anything up, so
/// `Metal\Wall01.vmt` and `metal/wall01` are one material. The filesystem is
/// case-insensitive on its own, so this is for the *cache*, not the lookup.
fn normalize_name(name: &str) -> String {
    let name: String = name
        .trim_matches(['/', '\\'])
        .chars()
        .map(|c| match c {
            '\\' => '/',
            c => c.to_ascii_lowercase(),
        })
        .collect();

    // `Q_StripExtension`: everything after the last `.`, but only if the `.` is
    // in the last path component. `props/pipe.001/base` keeps its name.
    match (name.rfind('.'), name.rfind('/')) {
        (Some(dot), slash) if slash.is_none_or(|slash| dot > slash) => name[..dot].to_owned(),
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalize_to_one_dictionary_key() {
        assert_eq!(normalize_name("Metal\\Wall01.vmt"), "metal/wall01");
        assert_eq!(normalize_name("metal/wall01"), "metal/wall01");
        assert_eq!(normalize_name("/METAL/Wall01.VMT"), "metal/wall01");
        // Only the last component's extension.
        assert_eq!(normalize_name("props/pipe.001/base"), "props/pipe.001/base");
        assert_eq!(
            normalize_name("props/pipe.001/base.vmt"),
            "props/pipe.001/base"
        );
    }

    #[test]
    fn the_error_material_is_a_valid_unlit_generic() {
        // It is built with `expect` at startup, so a typo in the literal above
        // would be a panic on every run. Check it here instead.
        let document = keyvalues::parse(ERROR_MATERIAL_NAME, ERROR_MATERIAL).unwrap();
        let vmt = Vmt::from_keyvalues(ERROR_MATERIAL_NAME, &document).unwrap();
        assert_eq!(
            ShaderKind::from_name(&vmt.shader),
            Some(ShaderKind::UnlitGeneric)
        );
        assert_eq!(
            vmt.var("$basetexture")
                .and_then(super::super::var::MaterialVar::as_str),
            Some("error")
        );
        assert!(vmt.flags.contains(MaterialFlags::MODEL));
    }

    /// Every `.vmt` in the shipped game loads, resolves to a real shader or to
    /// the error material on purpose, and builds a pipeline.
    ///
    /// The measurement `portdocs/MATERIALSYSTEM.md` §7.8 asks for — "verify
    /// against a real Portal 2 map's material list before committing" — run
    /// over the whole game rather than one map, and the thing that says a newly
    /// ported shader is actually finished. It prints a census by shader name,
    /// so a run is also the answer to "how much of the game does this port
    /// draw".
    ///
    /// Ignored by default and gated on `KISAK_GAME_DIR`, like the `studio/`,
    /// `world/` and `server/` depot tests. It needs a GPU, because building a
    /// pipeline is the half of this that has teeth:
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_materials -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn every_shipped_material_of_a_ported_shader_builds_a_pipeline() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            eprintln!("skipping: adapter has no BC texture support");
            return;
        }
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                ..Default::default()
            }))
        else {
            eprintln!("skipping: no usable device");
            return;
        };

        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        // Every `.vmt` under `materials/`, depth first. `Vfs::list` merges the
        // mounts, so this is the same set `FindFirst`/`FindNext` would walk.
        //
        // The names collected are **relative to `materials/`**, because that is
        // what `MaterialCache::load` takes: `Vmt::load` adds the directory and
        // the extension back on, the way `FindMaterial` does.
        let mut names = Vec::new();
        let mut directories = vec![String::new()];
        while let Some(directory) = directories.pop() {
            let listing = if directory.is_empty() {
                String::from("materials")
            } else {
                format!("materials/{directory}")
            };
            let Ok(entries) = vfs.list(&listing) else {
                continue;
            };
            for entry in entries {
                let path = if directory.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{directory}/{}", entry.name)
                };
                if entry.is_dir {
                    directories.push(path);
                } else if path.to_ascii_lowercase().ends_with(".vmt") {
                    names.push(normalize_name(&path));
                }
            }
        }
        names.sort();
        names.dedup();
        assert!(
            names.len() > 3000,
            "only {} materials found — is KISAK_GAME_DIR a Portal 2 install?",
            names.len()
        );

        let mut materials = MaterialCache::new(&device, &queue);
        let error = materials.error_material();
        let mut census: std::collections::BTreeMap<&'static str, (u32, u32)> =
            std::collections::BTreeMap::new();
        let mut unported = 0u32;
        let mut pipelines = std::collections::HashSet::new();
        // `$envmaptint`, which `vertex_lit_uniforms` decodes with
        // `GammaToLinearFullRange`. That decode is a bare `powf( 2.2 )` with no
        // domain guard — the same as C's `pow`, which is the point — so a
        // negative component would reach a uniform as a NaN. Counted here
        // because the claim that makes reproducing Valve exactly safe is a
        // claim about *content*: nothing in the shipped game writes one.
        let mut envmap_tints = 0u32;
        let mut negative_envmap_tints = Vec::<String>::new();
        // The translucent pass's own census: how much of the game blends, in
        // which mode, and how much is translucent only because `$translucent`
        // is set over a texture with no alpha channel — the one term of
        // `CMaterial::IsTranslucent` that the blend mode does not imply.
        let mut blends: std::collections::BTreeMap<String, u32> = Default::default();
        let mut translucent = 0u32;
        let mut translucent_without_blending = 0u32;

        // Validation errors arrive through the uncaptured-error handler rather
        // than as a `Result`, so they are latched and asserted at the end.
        let failures = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        device.on_uncaptured_error({
            let failures = std::sync::Arc::clone(&failures);
            std::sync::Arc::new(move |error: wgpu::Error| {
                failures.lock().unwrap().push(error.to_string())
            })
        });

        let target = crate::materials::pipeline::TargetFormat {
            color: wgpu::TextureFormat::Bgra8UnormSrgb,
            depth: Some(crate::materials::target::DEPTH_FORMAT),
            samples: 1,
        };
        for name in &names {
            if let Ok(vmt) = Vmt::load(&vfs, name) {
                if let Some(tint) = vmt.var("$envmaptint") {
                    envmap_tints += 1;
                    let value = tint.as_vec4();
                    if value[..3].iter().any(|component| *component < 0.0) {
                        negative_envmap_tints.push(format!("{name} {:?}", &value[..3]));
                    }
                }
            }

            let material = materials.load(&vfs, name);
            if Arc::ptr_eq(&material, &error) {
                // Either a shader this port has not written, or a `.vmt` that
                // does not resolve. Both are expected and both are counted
                // rather than asserted on: `MaterialCache::load` cannot fail.
                unported += 1;
                continue;
            }
            let entry = census.entry(material.shader.name()).or_default();
            entry.0 += 1;
            if material.state.blend != BlendMode::None {
                *blends
                    .entry(format!("{:?}", material.state.blend))
                    .or_default() += 1;
            }
            if material.is_translucent() {
                translucent += 1;
                if material.state.blend == BlendMode::None {
                    translucent_without_blending += 1;
                }
            }
            let key = crate::materials::pipeline::PipelineKey {
                shader: material.shader,
                state: material.state,
                target,
            };
            if pipelines.insert(key) {
                entry.1 += 1;
            }
            materials.pipelines().get(&key);
        }

        println!("{} .vmt files under materials/", names.len());
        for (shader, (loaded, variants)) in &census {
            println!("  {loaded:5} {shader}  ({variants} pipelines)");
        }
        println!("  {unported:5} <the error material>");
        println!("{} pipelines for the whole set", pipelines.len());
        println!("{envmap_tints} materials define $envmaptint");
        println!("{translucent} materials draw in the translucent pass:");
        for (mode, count) in &blends {
            println!("  {count:5} blend {mode}");
        }
        println!("  {translucent_without_blending:5} $translucent with no blending");

        assert!(
            negative_envmap_tints.is_empty(),
            "a negative $envmaptint reaches `gamma_to_linear_full_range_param`'s \
             un-guarded `powf` as a NaN — either clamp it there or explain these:\n{}",
            negative_envmap_tints.join("\n")
        );

        let failures = failures.lock().unwrap();
        assert!(
            failures.is_empty(),
            "{} pipeline(s) failed validation:\n{}",
            failures.len(),
            failures.join("\n")
        );

        // The deliverable for each ported shader, as a floor rather than an
        // exact count so that a depot with the language DLCs mounted does not
        // fail. The `Refract` and `Phong` figures are stage 6's breadth work:
        // 37 and 317 materials in the game draw with them, and every one has
        // to build a pipeline.
        let at_least = |shader: &str, count: u32| {
            let (loaded, _) = census.get(shader).copied().unwrap_or_default();
            assert!(
                loaded >= count,
                "{shader}: {loaded} materials loaded, expected at least {count}"
            );
        };
        // **`VertexLitGeneric` and `Phong` are one number split in two.**
        // 1,135 `.vmt` files name `VertexLitGeneric` and `WantsPhongShader`
        // sends 317 of them to `Phong` (`shader::wants_phong`), so the two
        // floors together are what the single 1,108 floor used to be — and if
        // the redirect ever stopped firing, the first of these would rise and
        // the second would fail.
        at_least("VertexLitGeneric", 818);
        at_least("Phong", 317);
        at_least("LightmappedGeneric", 800);
        at_least("UnlitGeneric", 928);
        at_least("WorldVertexTransition", 18);
        at_least("Refract", 37);
    }
}
