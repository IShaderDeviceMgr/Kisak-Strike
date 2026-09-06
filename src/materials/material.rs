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
use super::pipeline::{BindLayouts, PipelineCache, RenderState};
use super::shader::{self, Lighting, ShaderKind, TextureDimension};
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
    pub state: RenderState,
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
        let shader = ShaderKind::from_name(&vmt.shader)?;

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

        let uniforms = match shader {
            ShaderKind::UnlitGeneric => {
                let block = shader::unlit_uniforms(vmt);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
            ShaderKind::LightmappedGeneric => {
                let block = shader::lightmapped_uniforms(vmt);
                create_uniform_buffer(device, queue, name, bytemuck::bytes_of(&block))
            }
            ShaderKind::VertexLitGeneric => {
                let block = shader::vertex_lit_uniforms(vmt);
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

        let base_texture = textures
            .iter()
            .find(|(request, _)| request.param == "$basetexture")
            .map(|(_, texture)| texture.as_ref());

        Some(Material {
            name: name.to_owned(),
            shader,
            flags: vmt.flags,
            state: shader::render_state(shader, vmt, base_texture),
            modulation: shader::modulation_color(shader, vmt),
            lighting: shader::lighting(shader, vmt),
            textures: textures.into_iter().map(|(_, texture)| texture).collect(),
            uniforms,
            bind_group,
        })
    }
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

        MaterialCache {
            device: device.clone(),
            queue: queue.clone(),
            textures,
            pipelines,
            materials: HashMap::new(),
            error: Arc::new(error),
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

        // Said once, at load, because it is otherwise invisible: a fifth of
        // Portal 2's models name a shader that this port draws with the wrong
        // one. See `shader::wants_phong`.
        if ShaderKind::from_name(&vmt.shader) == Some(ShaderKind::VertexLitGeneric)
            && shader::wants_phong(&vmt)
        {
            eprintln!(
                "source-engine: materials: {name}: $phong asks for the Phong shader, \
                 which is not ported; drawing as VertexLitGeneric without specular"
            );
        }

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
}
