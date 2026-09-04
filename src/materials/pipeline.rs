//! Render pipelines: the state a material asks for, and the cache that turns
//! it into a `wgpu::RenderPipeline`.
//!
//! Replaces `StateSnapshot_t` and `materialsystem/shaderapidx9/TransitionTable.cpp`
//! (1,317 lines). Valve invented immutable, deduplicated pipeline state objects
//! because D3D9 had none: the shadow phase hashed a `ShadowState_t` into a
//! snapshot index, and `TransitionTable` then computed the minimal sequence of
//! `SetRenderState` calls to get from any snapshot to any other.
//!
//! `wgpu` has the first half natively and does not need the second: a
//! `RenderPipeline` *is* a snapshot, and `set_pipeline` *is* the transition,
//! computed by the driver against the hardware's real cost model rather than by
//! a table we maintain. So this module is the snapshot half — a key, a
//! `HashMap`, and the descriptor construction in between — and the transition
//! half is deleted with nothing in its place.
//!
//! The dedup strategy is the one `TransitionTable` used and the reason §6 of
//! `portdocs/MATERIALSYSTEM.md` says to read that file before deleting it:
//! hash the *state*, not the pointer, so that a thousand materials asking for
//! ordinary opaque rendering share one object.

use std::collections::HashMap;
use std::sync::Arc;

use super::shader::{
    ShaderKind, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, BINDING_BUMP_SAMPLER,
    BINDING_BUMP_TEXTURE, BINDING_LIGHTMAP_SAMPLER, BINDING_LIGHTMAP_TEXTURE,
    BINDING_MATERIAL_UNIFORMS,
};

/// Fixed pipeline state, as a material asks for it.
///
/// Every field is something a `.vmt` flag or the blend evaluation sets in the
/// shadow phase — see [`render_state`](super::shader::render_state), which is
/// the only thing that should be building one of these. Nothing here is
/// per-frame or per-draw: that is the point of a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderState {
    pub blend: BlendMode,
    /// Back-face culling. `EnableCulling`, off for `$nocull`.
    pub cull: bool,
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: DepthFunc,
    pub depth_bias: DepthBias,
    /// Whether the alpha channel of the render target is written.
    ///
    /// Off by default, which is Valve's default and surprising:
    /// `CShaderShadowDX8::SetDefaultState` calls `EnableAlphaWrites( false )`
    /// (`shadershadowdx8.cpp:225`), and shaders turn it back on only for fully
    /// opaque materials so that the frame's alpha channel can hold something
    /// else — depth, for the underwater fog pass.
    pub write_alpha: bool,
    /// `EnableAlphaToCoverage`, for `$allowalphatocoverage`. Needs a
    /// multisampled target to do anything.
    pub alpha_to_coverage: bool,
}

impl Default for RenderState {
    /// `CShaderShadowDX8::SetDefaultState` (`shadershadowdx8.cpp:219`), which
    /// every shadow phase starts from.
    fn default() -> RenderState {
        RenderState {
            blend: BlendMode::None,
            cull: true,
            depth_test: true,
            depth_write: true,
            depth_func: DepthFunc::NearerOrEqual,
            depth_bias: DepthBias::None,
            write_alpha: false,
            alpha_to_coverage: false,
        }
    }
}

/// `BlendType_t` (`public/shaderlib/baseshader_declarations.h:34`), plus the
/// one mode that was not in that enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// `BT_NONE`. Opaque.
    None,
    /// `BT_BLEND`: `src * srcAlpha + dst * (1 - srcAlpha)`.
    Blend,
    /// `BT_ADD`: `src + dst`.
    Add,
    /// `BT_BLENDADD`: `src * srcAlpha + dst`.
    BlendAdd,
    /// `$multiply`: `dst * src`. Set outside `SetBlendingShadowState`, at the
    /// end of the shadow block (`vertexlitgeneric_dx9_helper.cpp:1210`).
    Multiply,
}

impl BlendMode {
    /// The `wgpu` blend state, or `None` for opaque.
    ///
    /// D3D9 applied one pair of factors to all four channels unless
    /// `EnableBlendingSeparateAlpha` was on, and no shader in the target set
    /// turns that on — so colour and alpha take the same factors here, which is
    /// what "the same blend func" meant there.
    fn state(self) -> Option<wgpu::BlendState> {
        use wgpu::BlendFactor::{One, OneMinusSrcAlpha, Src, SrcAlpha, Zero};
        let (src, dst) = match self {
            BlendMode::None => return None,
            BlendMode::Blend => (SrcAlpha, OneMinusSrcAlpha),
            BlendMode::Add => (One, One),
            BlendMode::BlendAdd => (SrcAlpha, One),
            BlendMode::Multiply => (Zero, Src),
        };
        let component = wgpu::BlendComponent {
            src_factor: src,
            dst_factor: dst,
            operation: wgpu::BlendOperation::Add,
        };
        Some(wgpu::BlendState {
            color: component,
            alpha: component,
        })
    }
}

/// The two depth comparisons any shader in the target set asks for.
///
/// `ShaderDepthFunc_t` has eight (`public/shaderapi/ishadershadow.h:31`); the
/// rest are set by the flashlight and shadow passes, which are unported. The
/// names are Valve's and mean what they say: "nearer" passes when the fragment
/// is closer, which on a conventional depth buffer is `Less`
/// (`shadershadowdx8.cpp:288`, taking the non-reversed branch — `bReverseDepth`
/// is a debug convar and `ReverseDepthOnX360` is a console).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepthFunc {
    /// `SHADER_DEPTHFUNC_NEARER`, `$znearer`.
    Nearer,
    /// `SHADER_DEPTHFUNC_NEAREROREQUAL`, the default.
    NearerOrEqual,
}

impl From<DepthFunc> for wgpu::CompareFunction {
    fn from(func: DepthFunc) -> Self {
        match func {
            DepthFunc::Nearer => wgpu::CompareFunction::Less,
            DepthFunc::NearerOrEqual => wgpu::CompareFunction::LessEqual,
        }
    }
}

/// `ShaderPolyOffsetMode_t`, minus the shadow-bias mode that belongs to the
/// unported shadow-depth pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepthBias {
    /// `SHADER_POLYOFFSET_DISABLE`.
    None,
    /// `SHADER_POLYOFFSET_DECAL`: pull decals toward the eye so they win the
    /// depth test against the surface they sit on.
    Decal,
}

impl DepthBias {
    /// `CShaderAPIDx8::ApplyZBias` (`shaderapidx8.cpp:6232`) with the non-OSX
    /// convar defaults, `mat_slopescaledepthbias_decal -2` and
    /// `mat_depthbias_decal -0.0000038` (`shaderapidx8.cpp:6205`).
    ///
    /// The units differ and the conversion is the interesting part:
    /// `D3DRS_DEPTHBIAS` is a float in *normalized* depth, while `wgpu`'s
    /// `constant` is in units of the depth format's smallest representable
    /// increment. Against a 24-bit buffer that is `-0.0000038 * 2^24` = -63.75,
    /// rounded to -64.
    ///
    /// (The OSX defaults in the same file are -4 and **-0.25**, five orders of
    /// magnitude apart from the others — that is a GL driver whose bias was in
    /// depth units already, not a different artistic choice.)
    fn state(self) -> wgpu::DepthBiasState {
        match self {
            DepthBias::None => wgpu::DepthBiasState::default(),
            DepthBias::Decal => wgpu::DepthBiasState {
                constant: -64,
                slope_scale: -2.0,
                clamp: 0.0,
            },
        }
    }
}

/// What a pipeline is drawing *into*.
///
/// Part of the key because it is part of the pipeline: a `RenderPipeline`
/// declares its colour format, its depth format and its sample count, and using
/// it against anything else is a validation error rather than a slow path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetFormat {
    pub color: wgpu::TextureFormat,
    /// `None` until there is a depth buffer to attach — which is stage 4's
    /// render-target work (`portdocs/MATERIALSYSTEM.md` §9). With no depth
    /// attachment, [`RenderState`]'s depth fields are carried but not applied;
    /// they become live the day this is `Some`, with no other change.
    pub depth: Option<wgpu::TextureFormat>,
    pub samples: u32,
}

/// Everything that identifies one `wgpu::RenderPipeline`.
///
/// `StateSnapshot_t`, with the shader and the target folded in — they were
/// separate in D3D9 because binding a shader and setting render state were
/// separate calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    pub shader: ShaderKind,
    pub state: RenderState,
    pub target: TargetFormat,
}

/// The bind group layouts every pipeline shares.
///
/// Groups 0 and 2 are the same for every shader — that is what makes them
/// worth having as groups, since a shader change does not invalidate the
/// frame's bindings. Group 1 is the shader's own.
///
/// See [`uniforms`](super::uniforms) for what each group holds and why.
pub struct BindLayouts {
    frame: wgpu::BindGroupLayout,
    draw: wgpu::BindGroupLayout,
    unlit_material: wgpu::BindGroupLayout,
    lightmapped_material: wgpu::BindGroupLayout,
    lightmap: wgpu::BindGroupLayout,
}

impl BindLayouts {
    pub fn new(device: &wgpu::Device) -> BindLayouts {
        BindLayouts {
            frame: uniform_layout(device, "frame"),
            draw: uniform_layout(device, "draw"),
            unlit_material: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("material: UnlitGeneric"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: BINDING_MATERIAL_UNIFORMS,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: BINDING_BASE_TEXTURE,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: BINDING_BASE_SAMPLER,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            }),
            lightmapped_material: device.create_bind_group_layout(
                &wgpu::BindGroupLayoutDescriptor {
                    label: Some("material: LightmappedGeneric"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: BINDING_MATERIAL_UNIFORMS,
                            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        texture_entry(BINDING_BASE_TEXTURE),
                        sampler_entry(BINDING_BASE_SAMPLER),
                        texture_entry(BINDING_BUMP_TEXTURE),
                        sampler_entry(BINDING_BUMP_SAMPLER),
                    ],
                },
            ),
            lightmap: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("lightmap page"),
                entries: &[
                    texture_entry(BINDING_LIGHTMAP_TEXTURE),
                    sampler_entry(BINDING_LIGHTMAP_SAMPLER),
                ],
            }),
        }
    }

    /// Group 0: [`FrameUniforms`](super::uniforms::FrameUniforms).
    pub fn frame(&self) -> &wgpu::BindGroupLayout {
        &self.frame
    }

    /// Group 2: [`DrawUniforms`](super::uniforms::DrawUniforms).
    pub fn draw(&self) -> &wgpu::BindGroupLayout {
        &self.draw
    }

    /// Group 1, for one shader.
    pub fn material(&self, shader: ShaderKind) -> &wgpu::BindGroupLayout {
        match shader {
            ShaderKind::UnlitGeneric => &self.unlit_material,
            ShaderKind::LightmappedGeneric => &self.lightmapped_material,
        }
    }

    /// Group 3, for the shaders that read a lightmap page.
    ///
    /// One layout rather than one per shader, because the page is not the
    /// shader's: it is render-context state that several lit shaders will
    /// share. See
    /// [`BINDING_LIGHTMAP_TEXTURE`](super::shader::BINDING_LIGHTMAP_TEXTURE).
    pub fn lightmap(&self) -> &wgpu::BindGroupLayout {
        &self.lightmap
    }
}

/// A filterable 2D texture at `binding`.
const fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// The sampler that goes with it, one binding later.
const fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// A layout holding one uniform buffer at binding 0, visible to both stages,
/// bound with a dynamic offset.
///
/// The dynamic offset is what makes groups 0 and 2 sub-allocations of one
/// buffer rather than a buffer each: `Queue::write_buffer` stages its copy
/// ahead of the whole command buffer, so a block rewritten between draws would
/// be read by every draw in the frame at its final value. See
/// [`context`](super::context)'s "Uniforms are arenas" section, which is the
/// full statement of the hazard.
fn uniform_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Name → pipeline, built on demand and kept.
///
/// Compiling a shader module and creating a pipeline are the two expensive
/// calls in `wgpu`, and both are hidden behind this. There is no eviction:
/// §7.3 predicts single-digit pipeline counts per shader, so the whole cache is
/// tens of objects. If that prediction turns out wrong — the open question in
/// §10 — the fix is an on-disk `wgpu::PipelineCache`, and this is where it
/// goes.
pub struct PipelineCache {
    device: wgpu::Device,
    layouts: BindLayouts,
    /// One compiled module per shader, shared by all of its pipelines.
    modules: HashMap<ShaderKind, wgpu::ShaderModule>,
    pipelines: HashMap<PipelineKey, Arc<wgpu::RenderPipeline>>,
}

impl PipelineCache {
    pub fn new(device: &wgpu::Device) -> PipelineCache {
        PipelineCache {
            layouts: BindLayouts::new(device),
            device: device.clone(),
            modules: HashMap::new(),
            pipelines: HashMap::new(),
        }
    }

    pub fn layouts(&self) -> &BindLayouts {
        &self.layouts
    }

    /// The pipeline for a key, compiling and building it the first time.
    ///
    /// Returns an `Arc` so a caller can hold it across the borrow of the cache
    /// — which matters, because recording a pass borrows the frame and asking
    /// for a pipeline borrows this.
    pub fn get(&mut self, key: &PipelineKey) -> Arc<wgpu::RenderPipeline> {
        if let Some(pipeline) = self.pipelines.get(key) {
            return Arc::clone(pipeline);
        }

        let module = self.modules.entry(key.shader).or_insert_with(|| {
            self.device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(key.shader.name()),
                    source: wgpu::ShaderSource::Wgsl(key.shader.wgsl().into()),
                })
        });

        // Group 3 exists only for the shaders that read a lightmap page. A
        // pipeline layout is per shader, so declaring it unconditionally would
        // oblige every draw of every shader to bind something there.
        let mut groups = vec![
            Some(&self.layouts.frame),
            Some(self.layouts.material(key.shader)),
            Some(&self.layouts.draw),
        ];
        if key.shader.reads_lightmap() {
            groups.push(Some(&self.layouts.lightmap));
        }
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(key.shader.name()),
                bind_group_layouts: &groups,
                // No immediate (push-constant) data. Everything per-draw is in
                // group 2, which is what `portdocs/MATERIALSYSTEM.md` §7.4's
                // frequency split asks for.
                immediate_size: 0,
            });

        let state = key.state;
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(key.shader.name()),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    // `IShaderShadow::VertexShaderVertexFormat`: the layout is
                    // the shader's declaration, not the mesh's. See
                    // `ShaderKind::vertex_layout`.
                    buffers: &[Some(key.shader.vertex_layout().buffer_layout())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: key.target.color,
                        blend: state.blend.state(),
                        write_mask: if state.write_alpha {
                            wgpu::ColorWrites::ALL
                        } else {
                            wgpu::ColorWrites::COLOR
                        },
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    // Source's winding. `CShaderAPIDx8` sets `D3DCULL_CCW`,
                    // which culls *counter-clockwise* faces in D3D9's
                    // left-handed convention — the same triangles `wgpu` culls
                    // with `front_face: Ccw, cull_mode: Back`.
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: state.cull.then_some(wgpu::Face::Back),
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: key.target.depth.map(|format| wgpu::DepthStencilState {
                    format,
                    depth_write_enabled: Some(state.depth_write),
                    // A pipeline with no depth test still has a compare
                    // function; `Always` is how WebGPU spells "disabled".
                    depth_compare: Some(if state.depth_test {
                        state.depth_func.into()
                    } else {
                        wgpu::CompareFunction::Always
                    }),
                    stencil: wgpu::StencilState::default(),
                    bias: state.depth_bias.state(),
                }),
                multisample: wgpu::MultisampleState {
                    count: key.target.samples,
                    mask: !0,
                    alpha_to_coverage_enabled: state.alpha_to_coverage,
                },
                multiview_mask: None,
                cache: None,
            });

        let pipeline = Arc::new(pipeline);
        self.pipelines.insert(*key, Arc::clone(&pipeline));
        pipeline
    }

    /// How many distinct pipelines exist. §10 asks how many variants really
    /// survive the combo cull; this is the measurement, and the dedup test at
    /// the bottom of `preview.rs` is its only caller until something wants to
    /// report it.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.pipelines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_state_is_valves_default_state() {
        // `CShaderShadowDX8::SetDefaultState`, field by field. Every shadow
        // phase starts here, so a wrong default is wrong everywhere at once.
        let state = RenderState::default();
        assert_eq!(state.blend, BlendMode::None);
        assert!(state.cull);
        assert!(state.depth_test);
        assert!(state.depth_write);
        assert_eq!(state.depth_func, DepthFunc::NearerOrEqual);
        assert_eq!(state.depth_bias, DepthBias::None);
        assert!(!state.write_alpha, "EnableAlphaWrites( false )");
        assert!(!state.alpha_to_coverage);
    }

    #[test]
    fn blend_modes_map_to_the_d3d9_factor_pairs() {
        use wgpu::BlendFactor::*;

        assert!(BlendMode::None.state().is_none());

        let blend = BlendMode::Blend.state().unwrap();
        assert_eq!(blend.color.src_factor, SrcAlpha);
        assert_eq!(blend.color.dst_factor, OneMinusSrcAlpha);
        // D3D9 applies one pair to every channel unless separate alpha
        // blending is enabled, and nothing in the target set enables it.
        assert_eq!(blend.alpha, blend.color);

        assert_eq!(BlendMode::Add.state().unwrap().color.src_factor, One);
        assert_eq!(BlendMode::Add.state().unwrap().color.dst_factor, One);
        assert_eq!(
            BlendMode::BlendAdd.state().unwrap().color.src_factor,
            SrcAlpha
        );
        assert_eq!(BlendMode::BlendAdd.state().unwrap().color.dst_factor, One);
        assert_eq!(BlendMode::Multiply.state().unwrap().color.src_factor, Zero);
        assert_eq!(BlendMode::Multiply.state().unwrap().color.dst_factor, Src);
    }

    #[test]
    fn nearer_is_less_because_source_does_not_reverse_depth() {
        assert_eq!(
            wgpu::CompareFunction::from(DepthFunc::Nearer),
            wgpu::CompareFunction::Less
        );
        assert_eq!(
            wgpu::CompareFunction::from(DepthFunc::NearerOrEqual),
            wgpu::CompareFunction::LessEqual
        );
    }

    #[test]
    fn the_shader_is_what_decides_the_vertex_layout() {
        // Not the mesh, and not a field of the key: the layout comes from
        // `ShaderKind::vertex_layout`, which is `VertexShaderVertexFormat`.
        // Two keys that differ only in shader therefore differ in layout too,
        // which is why the key does not carry one.
        assert_eq!(
            ShaderKind::UnlitGeneric.vertex_layout(),
            crate::materials::mesh::VertexLayout::Simple
        );
    }

    #[test]
    fn keys_that_differ_only_in_state_are_different_pipelines() {
        let opaque = PipelineKey {
            shader: ShaderKind::UnlitGeneric,
            state: RenderState::default(),
            target: TargetFormat {
                color: wgpu::TextureFormat::Rgba8Unorm,
                depth: None,
                samples: 1,
            },
        };
        let mut translucent = opaque;
        translucent.state.blend = BlendMode::Blend;
        assert_ne!(opaque, translucent);

        let mut other_target = opaque;
        other_target.target.color = wgpu::TextureFormat::Bgra8UnormSrgb;
        assert_ne!(opaque, other_target);

        // And a key equal in every field is the same key, which is the whole
        // basis of the cache.
        assert_eq!(opaque, opaque);
    }
}
