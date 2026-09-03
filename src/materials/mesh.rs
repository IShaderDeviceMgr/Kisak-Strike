//! Geometry: what a vertex is, where vertices and indices live, and how a
//! draw names a range of them.
//!
//! Replaces `public/materialsystem/imesh.h` — 4,402 lines, of which about 3,900
//! are the inlined `CMeshBuilder`/`CVertexBuilder`/`CIndexBuilder` that write
//! one attribute at a time through a `VertexFormat_t` bitfield decoded at
//! runtime. None of that is ported. A vertex here is a `#[repr(C)]` struct, its
//! GPU layout is derived from the struct rather than reconstructed from a
//! `uint64`, and filling a buffer is `&[V]` -> `bytemuck::cast_slice`.
//!
//! # Vertex buffers and index buffers are separate, deliberately
//!
//! `IMesh` inherits from both `IVertexBuffer` and `IIndexBuffer`, so Valve's
//! unit of geometry is one object holding both. **Every real draw path in the
//! engine works around that**, which is why `GetDynamicMesh` grew
//! `vertexOverride`/`indexOverride` parameters:
//!
//! - World brushes: the vertices of every surface sharing a material are built
//!   into one static buffer at map load (`engine/matsys_interface.cpp:1864`),
//!   and each frame the *visible* surfaces' indices are gathered into a dynamic
//!   buffer — `GetDynamicMesh( false, g_WorldStaticMeshes[sortID] )`
//!   (`engine/gl_rsurf.cpp:1168`).
//! - Models: identical shape. `GetDynamicMeshEx( fmt, false, 0, pGroup->m_pMesh )`
//!   (`studiorender/r_studiodraw.cpp:2268`) over the static mesh group built by
//!   `R_StudioCreateStaticMeshes`.
//!
//! Static vertices plus dynamic indices is *the* pattern, not a special case.
//! So there is no `Mesh` type here: a draw takes a [`VertexSlice`] and an
//! [`IndexSlice`], and where they came from is not its business. That is the
//! main thing reading `studiorender/` and `engine/`'s draw paths changed about
//! this API, which is why `portdocs/MATERIALSYSTEM.md` §9 says to read them
//! first.
//!
//! # Vertex compression is not implemented
//!
//! `VERTEX_FORMAT_COMPRESSED` packs normals into 4 bytes and bone weights into
//! 2 (`common_vs_fxc.h`'s `DecompressBoneWeights` unpacks them). §10 asks
//! whether to keep it; nothing in the current shader set declares it —
//! `VertexLitGeneric` is the first that will
//! (`vertexlitgeneric_dx9_helper.cpp:893`) — so the question is still open and
//! is answered with skinning, not before it. The packing and the unpacking are
//! two halves of one decision.

use bytemuck::{Pod, Zeroable};

/// A vertex layout, as a shader declares it.
///
/// This is `VertexFormat_t` (`public/materialsystem/imesh.h:41`), a `uint64` of
/// flags plus per-texcoord sizes, reduced to the thing it was really used for.
/// It is an enum rather than a bitfield because the set is not open: a layout
/// only exists if some shader reads it, and a shader declares exactly one —
/// `pShaderShadow->VertexShaderVertexFormat( flags, nTexCoords, pDims, nUserData )`
/// in its shadow phase. See [`ShaderKind::vertex_layout`](super::shader::ShaderKind::vertex_layout).
///
/// The layouts the shipped shader set asks for, from that call in each helper —
/// recorded here because enumerating them is the expensive half of stage 4's
/// reading and the structs arrive with the shaders that read them:
///
/// | Shader | `VertexShaderVertexFormat` call | Attributes |
/// |---|---|---|
/// | `UnlitGeneric` | position, 1 texcoord, colour | [`Simple`](VertexLayout::Simple) |
/// | `LightmappedGeneric` (`lightmappedgeneric_dx9_helper.cpp:613,681`) | `VERTEX_POSITION`, 2 texcoords; `+TANGENT_S|TANGENT_T|NORMAL` and a 3rd texcoord when bumped; `+VERTEX_COLOR` for `$vertexcolor`/`$basetexture2` | world: position, base uv, lightmap uv, lightmap-offset uv, normal, tangent s/t, colour (`engine/matsys_interface.cpp:1552` writes exactly these) |
/// | `VertexLitGeneric` (`vertexlitgeneric_dx9_helper.cpp:769,895`) | `VERTEX_POSITION|VERTEX_NORMAL`, 1 texcoord, 4 floats of user data (tangent) when bumped, `VERTEX_FORMAT_COMPRESSED` | model: `mstudiovertex_t` (`public/studio.h:1447`) — bone weights, position, normal, uv — plus the `.vvd` tangent |
///
/// Note what the bumped column means and why `LightmappedGeneric` is the shader
/// §10 expects to force the variant question: bump mapping changes the *layout*,
/// so it is a pipeline variant before it is anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertexLayout {
    /// Position, one texture coordinate, one colour.
    ///
    /// What `unlitgeneric_vs20.fxc`'s `VS_INPUT` actually reads, and what the
    /// engine's own immediate-mode geometry is: screen quads
    /// (`DrawScreenSpaceQuad`), sprites, debug overlays, and the fade quad at
    /// `engine/gl_rmain.cpp:926`.
    Simple,
}

impl VertexLayout {
    /// The `wgpu` description of this layout.
    ///
    /// `@location` numbers match `shaders/prelude.wgsl`'s `VertexInput`; the
    /// two are one ABI and drift between them is a pipeline-creation error
    /// rather than a wrong picture, because `naga` checks that every location a
    /// shader declares is supplied.
    pub fn buffer_layout(self) -> wgpu::VertexBufferLayout<'static> {
        match self {
            VertexLayout::Simple => SimpleVertex::LAYOUT,
        }
    }

    /// Bytes per vertex.
    pub fn stride(self) -> u64 {
        match self {
            VertexLayout::Simple => size_of::<SimpleVertex>() as u64,
        }
    }
}

/// A type that can fill a vertex buffer.
///
/// Implemented by the `#[repr(C)]` structs below. The bound is what makes
/// `VertexBuffer::new` refuse anything whose bytes are not well-defined —
/// padding in a vertex struct reaches the GPU as whatever was on the stack.
pub trait Vertex: Pod {
    /// Which layout this struct is. Ties the Rust type to the enum a
    /// [`VertexBuffer`] records and a shader declares.
    const LAYOUT: VertexLayout;
}

/// Position, texture coordinate, colour. [`VertexLayout::Simple`].
///
/// Valve's is object space, transformed in the vertex shader by `SkinPosition`
/// against `cModel[0]` even when nothing is skinned (`common_vs_fxc.h:170`);
/// here the object-to-world transform is
/// [`DrawUniforms::model`](super::uniforms::DrawUniforms::model) and does the
/// same job without the skinning path.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct SimpleVertex {
    pub position: [f32; 3],
    /// `TEXCOORD0`.
    pub texcoord: [f32; 2],
    /// `COLOR0`, read only when `$vertexcolor` is set.
    pub color: [f32; 4],
}

impl SimpleVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x2,
        2 => Float32x4,
    ];

    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<SimpleVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &SimpleVertex::ATTRIBUTES,
    };

    /// White, at the origin. A base to override fields of, so that adding an
    /// attribute to this struct does not have to be echoed at every literal.
    pub const fn new(position: [f32; 3], texcoord: [f32; 2]) -> SimpleVertex {
        SimpleVertex {
            position,
            texcoord,
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

impl Vertex for SimpleVertex {
    const LAYOUT: VertexLayout = VertexLayout::Simple;
}

/// Vertices that live on the GPU for longer than a frame.
///
/// `CreateStaticVertexBuffer` / `CreateStaticMesh`'s vertex half. Immutable
/// once built, which is what every caller of those actually wanted: the world's
/// vertices are written once at map load and read for the lifetime of the map.
pub struct VertexBuffer {
    buffer: wgpu::Buffer,
    layout: VertexLayout,
    count: u32,
}

// The accessors have no caller until something asks a buffer about itself --
// `mem_dumpvballocs`, or a batcher checking a layout before it builds indices.
#[allow(dead_code)]
impl VertexBuffer {
    /// Uploads `vertices`, which may not be empty.
    ///
    /// `wgpu` refuses a zero-sized buffer, and an empty static buffer is a
    /// caller bug rather than a state to represent — the reference asserts the
    /// same thing (`Assert( g_Meshes[i].vertCount > 0 )`,
    /// `engine/matsys_interface.cpp:1861`).
    pub fn new<V: Vertex>(device: &wgpu::Device, label: &str, vertices: &[V]) -> VertexBuffer {
        assert!(!vertices.is_empty(), "{label}: empty vertex buffer");
        VertexBuffer {
            buffer: upload(
                device,
                label,
                wgpu::BufferUsages::VERTEX,
                bytemuck::cast_slice(vertices),
            ),
            layout: V::LAYOUT,
            count: vertices.len() as u32,
        }
    }

    pub fn layout(&self) -> VertexLayout {
        self.layout
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The whole buffer, as something a draw can take.
    pub fn slice(&self) -> VertexSlice {
        VertexSlice {
            buffer: self.buffer.clone(),
            layout: self.layout,
            offset: 0,
            count: self.count,
        }
    }
}

/// Indices that live on the GPU for longer than a frame.
pub struct IndexBuffer {
    buffer: wgpu::Buffer,
    count: u32,
}

#[allow(dead_code)]
impl IndexBuffer {
    /// Uploads `indices`, which may not be empty.
    ///
    /// 16-bit only, as `IndexDesc_t::m_pIndices` is
    /// (`public/materialsystem/imesh.h:167`): `MATERIAL_INDEX_FORMAT_32BIT`
    /// exists in the enum but nothing in the engine's draw paths asks for it,
    /// because a batch is bounded by `GetMaxIndicesToRender` long before it
    /// reaches 65,536 vertices. Adding it later is a field here and a
    /// `wgpu::IndexFormat` in [`IndexSlice`].
    pub fn new(device: &wgpu::Device, label: &str, indices: &[u16]) -> IndexBuffer {
        assert!(!indices.is_empty(), "{label}: empty index buffer");
        IndexBuffer {
            buffer: upload(
                device,
                label,
                wgpu::BufferUsages::INDEX,
                bytemuck::cast_slice(indices),
            ),
            count: indices.len() as u32,
        }
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The whole buffer, as something a draw can take.
    pub fn slice(&self) -> IndexSlice {
        IndexSlice {
            buffer: self.buffer.clone(),
            offset: 0,
            count: self.count,
        }
    }

    /// A sub-range, in indices.
    ///
    /// `IMesh::Draw( nFirstIndex, nIndexCount )`, which the world renderer uses
    /// to draw one material's batch out of a shared buffer
    /// (`engine/gl_rsurf.cpp:1438`).
    pub fn range(&self, first: u32, count: u32) -> IndexSlice {
        assert!(
            first.saturating_add(count) <= self.count,
            "index range {first}..{} is outside a buffer of {}",
            first + count,
            self.count
        );
        IndexSlice {
            buffer: self.buffer.clone(),
            // Times two because `set_index_buffer` takes bytes.
            offset: u64::from(first) * 2,
            count,
        }
    }
}

/// A range of vertices to draw from, whoever owns them.
///
/// Holds a cloned `wgpu::Buffer`, which is a refcounted handle rather than the
/// allocation — cloning is one atomic increment. That is what lets a slice
/// outlive the borrow of a [`DynamicBuffers`] arena it came from, and it is
/// what makes "static vertices, dynamic indices" a pair of ordinary values
/// instead of the two override parameters `GetDynamicMesh` grew.
#[derive(Debug, Clone)]
pub struct VertexSlice {
    buffer: wgpu::Buffer,
    layout: VertexLayout,
    offset: u64,
    count: u32,
}

#[allow(dead_code)]
impl VertexSlice {
    pub fn layout(&self) -> VertexLayout {
        self.layout
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(super) fn buffer_slice(&self) -> wgpu::BufferSlice<'_> {
        let bytes = u64::from(self.count) * self.layout.stride();
        self.buffer.slice(self.offset..self.offset + bytes)
    }
}

/// A range of indices to draw.
#[derive(Debug, Clone)]
pub struct IndexSlice {
    buffer: wgpu::Buffer,
    offset: u64,
    count: u32,
}

impl IndexSlice {
    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(super) fn buffer_slice(&self) -> wgpu::BufferSlice<'_> {
        let bytes = u64::from(self.count) * 2;
        self.buffer.slice(self.offset..self.offset + bytes)
    }
}

/// How much a fresh arena holds, in bytes, for each of vertices and indices.
///
/// `shaderapidx9/dynamicvb.h` sized its dynamic buffers by a
/// `DYNAMIC_VERTEX_BUFFER_MEMORY` constant of the same order (1 MB, quartered
/// on the 360). The number matters less here than it did there, because
/// [`DynamicBuffers`] grows: this is the size at which no growth is needed for
/// an ordinary frame, not a ceiling.
const ARENA_BYTES: u64 = 1 << 20;

/// Per-frame geometry: written this frame, drawn this frame, forgotten.
///
/// `CShaderAPIDx8`'s dynamic vertex and index buffers
/// (`shaderapidx9/dynamicvb.h`, `dynamicib.h`) — a large buffer sub-allocated
/// with `D3DLOCK_NOOVERWRITE` until it fills, then rotated with
/// `D3DLOCK_DISCARD`. The reasoning survives and the code does not: `wgpu`'s
/// `Queue::write_buffer` stages the copy and orders it ahead of the submission
/// that reads it, so a bump allocator reset once a frame is the whole of it.
///
/// **Safety of the reset:** frame *n*'s draws are recorded and submitted before
/// frame *n+1* writes over offset 0, and queue submissions execute in order, so
/// the GPU has finished reading before the overwrite lands. Growing mid-frame is
/// safe for a different reason: the replaced `wgpu::Buffer` stays alive as long
/// as a recorded command buffer references it, and the slices already handed
/// out hold their own handles to it.
pub struct DynamicBuffers {
    vertices: Arena,
    indices: Arena,
    /// Reused so that padding an odd-length write does not allocate.
    scratch: Vec<u8>,
}

// `indices` and the two `*_remaining` queries are `GetDynamicMesh`'s index
// half and `GetMaxIndicesToRender`/`GetMaxVerticesToRender`. The world
// renderer reads them before every batch (`engine/gl_rsurf.cpp:1162`); nothing
// in this binary batches yet, and `Pass` is what re-exports them.
#[allow(dead_code)]
impl DynamicBuffers {
    pub fn new(device: &wgpu::Device) -> DynamicBuffers {
        DynamicBuffers {
            vertices: Arena::new(device, "dynamic vertices", wgpu::BufferUsages::VERTEX),
            indices: Arena::new(device, "dynamic indices", wgpu::BufferUsages::INDEX),
            scratch: Vec::new(),
        }
    }

    /// Drops everything written last frame and reclaims the space.
    ///
    /// Must be called once per frame, before anything is written. Anything
    /// still holding a slice from the previous frame will draw whatever
    /// overwrites it.
    pub fn begin_frame(&mut self, device: &wgpu::Device) {
        self.vertices.begin_frame(device);
        self.indices.begin_frame(device);
    }

    /// Writes vertices for this frame.
    pub fn vertices<V: Vertex>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[V],
    ) -> VertexSlice {
        let bytes = pad_to_copy_alignment(bytemuck::cast_slice(vertices), &mut self.scratch);
        let (buffer, offset) = self.vertices.write(device, queue, bytes);
        VertexSlice {
            buffer,
            layout: V::LAYOUT,
            offset,
            count: vertices.len() as u32,
        }
    }

    /// Writes indices for this frame.
    pub fn indices(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        indices: &[u16],
    ) -> IndexSlice {
        let bytes = pad_to_copy_alignment(bytemuck::cast_slice(indices), &mut self.scratch);
        let (buffer, offset) = self.indices.write(device, queue, bytes);
        IndexSlice {
            buffer,
            offset,
            count: indices.len() as u32,
        }
    }

    /// How many more vertices of a given layout fit in this frame.
    ///
    /// `IMatRenderContext::GetMaxVerticesToRender`. The world renderer reads
    /// the index equivalent before every batch (`engine/gl_rsurf.cpp:1162`) and
    /// splits when a batch would not fit, which is the behaviour this exists
    /// to support.
    pub fn vertices_remaining(&self, layout: VertexLayout) -> u32 {
        (self.vertices.remaining() / layout.stride()) as u32
    }

    /// How many more indices fit in this frame. `GetMaxIndicesToRender`.
    pub fn indices_remaining(&self) -> u32 {
        (self.indices.remaining() / 2) as u32
    }
}

/// One growable bump-allocated buffer.
///
/// Two counters, and the difference between them is the whole of the growth
/// policy. `used` is the offset into the buffer that exists now; `demand` is
/// what the frame has asked for in total, and it survives a mid-frame
/// reallocation so that the next frame sizes itself from the real figure rather
/// than from whatever happened after the last growth.
struct Arena {
    label: &'static str,
    usage: wgpu::BufferUsages,
    buffer: wgpu::Buffer,
    capacity: u64,
    used: u64,
    demand: u64,
}

#[allow(dead_code)]
impl Arena {
    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages) -> Arena {
        Arena {
            label,
            usage,
            buffer: empty(device, label, usage, ARENA_BYTES),
            capacity: ARENA_BYTES,
            used: 0,
            demand: 0,
        }
    }

    fn begin_frame(&mut self, device: &wgpu::Device) {
        // Grows but never shrinks: a frame that needed the space once will
        // very likely need it again, and reclaiming it costs a reallocation
        // for nothing. Same reasoning as `CDynamicVB`'s fixed allocation, with
        // the ceiling removed.
        if self.demand > self.capacity {
            self.grow(device, self.demand);
        }
        self.used = 0;
        self.demand = 0;
    }

    fn remaining(&self) -> u64 {
        self.capacity.saturating_sub(self.used)
    }

    /// Round up to a power of two so a frame that grows a little every frame
    /// does not reallocate every frame.
    fn grow(&mut self, device: &wgpu::Device, wanted: u64) {
        self.capacity = wanted.next_power_of_two();
        self.buffer = empty(device, self.label, self.usage, self.capacity);
    }

    /// Reserves and writes `bytes`, whose length is already a multiple of
    /// `COPY_BUFFER_ALIGNMENT`. Returns the buffer to draw from and the byte
    /// offset within it.
    fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bytes: &[u8],
    ) -> (wgpu::Buffer, u64) {
        let len = bytes.len() as u64;
        self.demand += len;

        if self.used + len > self.capacity {
            // Mid-frame growth: this frame wants more than any frame before
            // it. Size from `demand`, not from this one allocation, so a frame
            // that overflows early does not reallocate again for every write
            // after it. The replaced buffer stays alive for as long as the
            // draws already recorded against it, and the slices handed out
            // hold their own handles to it, so the reset to offset zero is
            // writing into genuinely fresh memory.
            self.grow(device, self.demand);
            self.used = 0;
        }

        let offset = self.used;
        queue.write_buffer(&self.buffer, offset, bytes);
        self.used += len;
        (self.buffer.clone(), offset)
    }
}

/// `Queue::write_buffer` requires both the offset and the length to be a
/// multiple of `COPY_BUFFER_ALIGNMENT` (4). Every allocation here is a multiple
/// of 4 long, so offsets take care of themselves; lengths do not, because an
/// odd number of 16-bit indices is two bytes short.
///
/// Returns the input untouched when it is already aligned, so the common case
/// does not copy.
fn pad_to_copy_alignment<'a>(bytes: &'a [u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
    let alignment = wgpu::COPY_BUFFER_ALIGNMENT as usize;
    let overhang = bytes.len() % alignment;
    if overhang == 0 {
        return bytes;
    }
    scratch.clear();
    scratch.extend_from_slice(bytes);
    scratch.resize(bytes.len() + (alignment - overhang), 0);
    scratch
}

fn empty(device: &wgpu::Device, label: &str, usage: wgpu::BufferUsages, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Creates a buffer holding `contents`.
///
/// `wgpu::util::DeviceExt::create_buffer_init` does this in one call;
/// `mapped_at_creation` is the same thing without depending on the `util`
/// module, and it is the one place in the module that writes to a buffer
/// without going through the queue.
///
/// The allocation is rounded up to `COPY_BUFFER_ALIGNMENT`, which
/// `mapped_at_creation` requires — an odd number of 16-bit indices is two bytes
/// short of it. The padding is never drawn, because the slice carries the real
/// count.
fn upload(
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    contents: &[u8],
) -> wgpu::Buffer {
    let alignment = wgpu::COPY_BUFFER_ALIGNMENT as usize;
    let size = contents.len().next_multiple_of(alignment);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage,
        mapped_at_creation: true,
    });
    let mut mapped = buffer
        .slice(..)
        .get_mapped_range_mut()
        .expect("a just-created mapped buffer");
    // `slice(..len)` rather than `copy_from_slice(contents)` on the whole
    // view: the allocation may be longer than the contents, by up to three
    // bytes of alignment padding.
    mapped.slice(..contents.len()).copy_from_slice(contents);
    drop(mapped);
    buffer.unmap();
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simple_vertex_has_no_padding() {
        // `Pod` already refuses a struct with padding, but the size is also
        // the vertex stride the GPU steps by, so it is worth stating: three
        // floats, two floats, four floats.
        assert_eq!(size_of::<SimpleVertex>(), (3 + 2 + 4) * 4);
        assert_eq!(
            VertexLayout::Simple.stride(),
            size_of::<SimpleVertex>() as u64
        );
    }

    #[test]
    fn the_declared_layout_covers_the_whole_vertex() {
        // A layout whose attributes do not reach the end of the struct is not
        // an error anywhere -- it just quietly stops feeding the shader.
        let layout = VertexLayout::Simple.buffer_layout();
        let last = layout
            .attributes
            .last()
            .expect("Simple declares attributes");
        assert_eq!(
            last.offset + 16,
            layout.array_stride,
            "the colour is the last attribute and is 16 bytes"
        );
        assert_eq!(layout.array_stride, VertexLayout::Simple.stride());
        assert_eq!(layout.array_stride, 36, "3 + 2 + 4 floats");
        assert_eq!(layout.attributes.len(), 3);
        assert_eq!(layout.attributes[0].offset, 0);
        assert_eq!(layout.attributes[1].offset, 12, "texcoord after position");
        assert_eq!(layout.attributes[2].offset, 20, "colour after texcoord");

        // Locations are dense from zero and match `prelude.wgsl`'s
        // `VertexInput`.
        let locations: Vec<u32> = layout
            .attributes
            .iter()
            .map(|a| a.shader_location)
            .collect();
        assert_eq!(locations, [0, 1, 2]);
    }

    #[test]
    fn padding_only_copies_when_it_has_to() {
        let mut scratch = Vec::new();

        // Four u16 indices: eight bytes, already aligned, no copy.
        let aligned: &[u16] = &[0, 1, 2, 3];
        let bytes = bytemuck::cast_slice(aligned);
        assert_eq!(pad_to_copy_alignment(bytes, &mut scratch).len(), 8);
        assert!(
            scratch.is_empty(),
            "the aligned case must not touch scratch"
        );

        // Three: six bytes, padded to eight with a zero index that is never
        // drawn because the slice's count is still three.
        let odd: &[u16] = &[7, 8, 9];
        let padded = pad_to_copy_alignment(bytemuck::cast_slice(odd), &mut scratch);
        assert_eq!(padded.len(), 8);
        assert_eq!(&padded[..6], bytemuck::cast_slice(odd));
        assert_eq!(&padded[6..], &[0, 0]);
    }

    #[test]
    fn padding_handles_every_remainder() {
        let mut scratch = Vec::new();
        for len in 0..16usize {
            let bytes = vec![0xABu8; len];
            let padded = pad_to_copy_alignment(&bytes, &mut scratch);
            assert!(
                padded
                    .len()
                    .is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize),
                "{len} bytes padded to {}",
                padded.len()
            );
            assert!(padded.len() >= len);
            assert!(padded.len() < len + wgpu::COPY_BUFFER_ALIGNMENT as usize);
            assert_eq!(&padded[..len], &bytes[..]);
        }
    }
}
