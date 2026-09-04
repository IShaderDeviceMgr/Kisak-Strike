//! Lightmaps: packing the `.bsp`'s per-face light samples into atlas pages.
//!
//! Replaces `materialsystem/cmatlightmaps.cpp` (2,465 lines),
//! `materialsystem/imagepacker.cpp` (169) and the lightmap half of
//! `materialsystem/colorspace.h`. `portdocs/MATERIALSYSTEM.md` §4.5 asks for
//! the packing algorithm to be ported faithfully, and it is: [`ImagePacker`]
//! is `CImagePacker` line for line, because the page a surface lands on and
//! the offset within it decide its texture coordinates, and any other packer
//! would produce a different — not wrong, but different — atlas.
//!
//! What is *not* ported, and why:
//!
//! | In the original | Here |
//! |---|---|---|
//! | `LockLightmap`/`UpdateLightmap`, the dynamic pages, `COUNT_DYNAMIC_LIGHTMAP_PAGES` | dynamic lights and lightstyle animation are not ported; a page is written once at load |
//! | `ReleaseLightmapPages`/`RestoreLightmapPages` | D3D9 lost-device handling, which `wgpu` does not have |
//! | `m_eLightmapsState`, `EnumerateMaterials`, `GetSortInfo` | the sort-ID table; here a batch *is* a sort ID and `src/engine/world/` owns the grouping |
//! | `mat_lightmap_pfms`, the `FloatBitMap_t` dumps | a debugging path for a format we do not write |
//! | `LightmapBitsToPixelWriter_LDR`/`_HDRI` and the X360/PS3 SIMD variants | one format, see below |
//!
//! # One page format: `Rgba16Float`, holding linear radiance
//!
//! Valve picked the page format from `GetHDRType()`
//! (`cmatlightmaps.cpp:481`): `RGBA8888` + sRGB read for LDR, `RGBA16161616`
//! for HDR-integer, `RGBA16161616F` for HDR-float. **Portal 2 ships HDR-only
//! maps** — `sp_a1_intro1.bsp` has an empty `LUMP_LIGHTING` and 5.4 MB of
//! `LUMP_LIGHTING_HDR` — so the LDR path is not an option, and of the two HDR
//! ones the float path is the one whose page contents are just *the numbers*:
//! `GetLightMapScaleFactor` is 1.0 for it (`hardwareconfig.cpp:832`) against
//! 16.0 for the integer path, so nothing is pre-divided and nothing has to be
//! multiplied back.
//!
//! So a page texel is linear radiance, in `Rgba16Float`, sampled without an
//! sRGB decode — which is what `BindStandardTexture( SHADER_SAMPLER1, bHDR ?
//! TEXTURE_BINDFLAGS_NONE : TEXTURE_BINDFLAGS_SRGBREAD, TEXTURE_LIGHTMAP )`
//! (`lightmappedgeneric_dx9_helper.cpp:583`) says. `Rgba16Float` is filterable
//! at the portable capability floor, which `Rgba16Unorm`
//! (`Features::TEXTURE_FORMAT_16BIT_NORM`) and `Rgba32Float`
//! (`Features::FLOAT32_FILTERABLE`) are not, so this costs nothing from
//! `portdocs/MATERIALSYSTEM.md` §4.6's single tier.

use std::sync::Arc;

use super::pipeline::BindLayouts;
use super::texture::Texture;

/// Page width. `CMatLightmaps::GetMaxLightmapPageWidth` (`cmatlightmaps.cpp:113`),
/// whose comment explains the number: 512 wide "because that's the only way
/// bumped lighting on displacements can work given the 128x128 allowance".
pub const PAGE_WIDTH: u32 = 512;

/// Page height. `GetMaxLightmapPageHeight` (`cmatlightmaps.cpp:134`).
pub const PAGE_HEIGHT: u32 = 256;

/// How many blocks wide a bumped surface's allocation is: the flat lightmap
/// plus one per basis vector.
///
/// `NUM_BUMP_VECTS + 1` (`public/mathlib/bumpvects.h`), as
/// `RegisterLightmappedSurface` (`engine/gl_matsysiface.cpp:235`) multiplies
/// the width by. The flat map is block 0 and the three directional ones follow
/// it left to right, which is what makes the shader's "add the offset three
/// times" work.
pub const BUMP_BLOCKS: u32 = 4;

/// One light sample in the `.bsp`: `ColorRGBExp32`
/// (`public/mathlib/mathlib.h:1470`).
///
/// RGB mantissas with one shared exponent, which is how `vrad` keeps the whole
/// dynamic range of a bounce solution in four bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct ColorRgbExp32 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub exponent: i8,
}

impl ColorRgbExp32 {
    /// The linear radiance this encodes, in the units a lightmap page holds.
    ///
    /// `TexLightToLinear( c, e )` (`public/mathlib/mathlib.h:1453`), which is
    /// `c * power2_n[e + 128]` against a table documented as `2^(index - 128) /
    /// 255` (`color_conversion.cpp:40`) — so `c * 2^e / 255`.
    ///
    /// **Not `ColorRGBExp32ToVector`**, which is the same thing times 255 and
    /// is a trap: it is the obvious-looking "decode this colour" function, it
    /// sits right next to the table, and Valve's own comment on the extra
    /// factor is *"FIXME: Why is there a factor of 255 built into this?"*. It
    /// is for `dworldlight_t` intensities, ambient cubes and particle colours
    /// (`modelloader.cpp:1858`, `:7338`); the lightmap path calls
    /// `TexLightToLinear` directly (`gl_lightmap.cpp:572`). Using the wrong one
    /// makes every lit surface 255 times too bright, which is a white screen
    /// rather than anything that reads as a decoding error.
    ///
    /// The resulting range is the `[0..16]` that
    /// `LightmapBitsToPixelWriter_HDRI`'s comment names.
    pub fn to_linear(self) -> [f32; 3] {
        let scale = exp2i(self.exponent) / 255.0;
        [
            f32::from(self.r) * scale,
            f32::from(self.g) * scale,
            f32::from(self.b) * scale,
        ]
    }
}

/// `2^n` for the exponent range a `ColorRGBExp32` can hold.
///
/// `power2_n[]` is a 256-entry table in the original because this is in the
/// inner loop of every lightmap rebuild; here it is called once per luxel at
/// map load, so `exp2` is cheaper than the cache line.
fn exp2i(exponent: i8) -> f32 {
    f32::exp2(f32::from(exponent))
}

/// A rectangle packer for one lightmap page.
///
/// `CImagePacker` (`materialsystem/imagepacker.cpp`), ported as-is. It is a
/// skyline packer: `wavefront[x]` is the highest occupied `y` in column `x`,
/// and a block goes at the leftmost position whose maximum wavefront over its
/// width is lowest.
///
/// **Ported faithfully on purpose.** The packer is not an implementation
/// detail: a face's page and its offset within that page *are* its lightmap
/// texture coordinates, so a different packer means a different atlas, a
/// different number of pages, and a different number of draw batches. Keeping
/// this one keeps those numbers comparable with the original.
pub struct ImagePacker {
    width: u32,
    height: u32,
    /// `m_pLightmapWavefront`. `-1` means the column is empty, so this is
    /// signed and one below the first row.
    wavefront: Vec<i32>,
    area_used: u32,
    /// `m_MinimumHeight`, the tallest occupied row plus one. Only read by
    /// [`efficiency`](ImagePacker::efficiency).
    minimum_height: i32,
    /// `m_MaxBlockWidth`/`m_MaxBlockHeight`: the size of the first block that
    /// failed to fit, so that a later block at least that big can be rejected
    /// without searching. Both start one past the page size, meaning "nothing
    /// has failed yet".
    max_block: (u32, u32),
}

impl ImagePacker {
    /// An empty page. `CImagePacker::Reset`.
    pub fn new(width: u32, height: u32) -> ImagePacker {
        ImagePacker {
            width,
            height,
            wavefront: vec![-1; width as usize],
            area_used: 0,
            minimum_height: -1,
            max_block: (width + 1, height + 1),
        }
    }

    /// Places a block, returning its top-left corner. `CImagePacker::AddBlock`.
    pub fn add_block(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        // "If we've already determined that a block this big couldn't fit then
        // blow off checking again..."
        if width >= self.max_block.0 && height >= self.max_block.1 {
            return None;
        }
        if width > self.width {
            self.note_failure(width, height);
            return None;
        }

        let mut best_x = None;
        let mut best_y = self.height as i32;
        let last_x = self.width - width;
        let mut outer_x = 0;
        let mut last_max_y = -2;

        while outer_x <= last_x {
            // "Skip all tiles that have the last Y value, these aren't going
            // to change our min Y value."
            if self.wavefront[outer_x as usize] == last_max_y {
                outer_x += 1;
                continue;
            }

            let max_y_index = self.max_y_index(outer_x, width);
            last_max_y = self.wavefront[max_y_index as usize];
            if best_y > last_max_y {
                best_y = last_max_y;
                best_x = Some(outer_x);
            }
            outer_x = max_y_index + 1;
        }

        let Some(x) = best_x else {
            self.note_failure(width, height);
            return None;
        };
        let y = best_y + 1;

        // Valve's height check, `>=` and all: it rejects a block that ends on
        // the last row as well as one that runs past it. Reproduced rather
        // than corrected, because loosening it would move blocks that the
        // original spilled onto the next page.
        if y + height as i32 >= self.height as i32 - 1 {
            self.note_failure(width, height);
            return None;
        }

        if y + height as i32 > self.minimum_height {
            self.minimum_height = y + height as i32;
        }
        for column in x..x + width {
            self.wavefront[column as usize] = best_y + height as i32;
        }
        self.area_used += width * height;

        Some((x, y as u32))
    }

    /// `CImagePacker::GetMaxYIndex` — the column with the highest wavefront in
    /// `[first_x, first_x + width)`, preferring the *last* such column.
    ///
    /// The `>=` is Valve's and is load-bearing; the comment there says why:
    /// "Want the equals here since we'll never be able to fit in between the
    /// multiple instances of maxY". It is what lets `add_block`'s loop jump
    /// past a whole run of equal columns instead of retrying each one.
    fn max_y_index(&self, first_x: u32, width: u32) -> u32 {
        let mut max_y = -1;
        let mut max_y_index = 0;
        for x in first_x..first_x + width {
            if self.wavefront[x as usize] >= max_y {
                max_y = self.wavefront[x as usize];
                max_y_index = x;
            }
        }
        max_y_index
    }

    /// "If we failed to add it, remember the block size that failed *only if
    /// both dimensions are smaller*!! Just because a 1x10 block failed,
    /// doesn't mean a 10x1 block will fail."
    fn note_failure(&mut self, width: u32, height: u32) {
        if width <= self.max_block.0 && height <= self.max_block.1 {
            self.max_block = (width, height);
        }
    }

    /// Fraction of the page's used rows that hold a block.
    /// `CImagePacker::GetEfficiency`, reported by `mat_info`.
    #[allow(dead_code)]
    pub fn efficiency(&self) -> f32 {
        let used_height = (self.minimum_height.max(0) as u32).next_power_of_two().max(1);
        self.area_used as f32 / (self.width * used_height) as f32
    }
}

/// Where one surface's lightmap ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// Index into [`LightmapPages`].
    pub page: u32,
    /// Top-left corner of the *flat* block, in texels.
    ///
    /// `MSurf_OffsetIntoLightmapPage`. A bumped surface's three directional
    /// blocks follow at `x + width`, `x + 2*width`, `x + 3*width`.
    pub x: u32,
    pub y: u32,
}

/// The lightmap page a surface with no light samples binds.
///
/// `MATERIAL_SYSTEM_LIGHTMAP_PAGE_WHITE` (`imaterialsystem.h`), which
/// `RegisterUnlightmappedSurface` (`gl_matsysiface.cpp:256`) hands to any
/// surface whose `texinfo` says `SURF_NOLIGHT` or whose `light_ofs` is -1.
/// Valve made it a negative page ID rather than a page; here it is a real 1x1
/// page so that nothing downstream has to special-case it, and it is page 0 of
/// every atlas.
pub const WHITE_PAGE: u32 = 0;

/// Assigns surfaces to pages. `CMatLightmaps`' allocation half.
///
/// The material-change rule is Valve's and is the reason this is a state
/// machine rather than a function: [`begin_material`](LightmapAllocator::begin_material)
/// closes every open page but the last, so that a material's surfaces land on
/// as few pages as possible and each (material, page) pair is one draw batch.
/// `AllocateLightmap` (`cmatlightmaps.cpp:306`) does it by removing image
/// packers from the list; the comment there — "we need to close out all image
/// packers other than the last one so as to produce as few sort IDs as
/// possible" — is the whole design.
pub struct LightmapAllocator {
    /// Open pages, oldest first, each paired with its page index. Page 0 is
    /// [`WHITE_PAGE`] and is never in here.
    open: Vec<(u32, ImagePacker)>,
    page_count: u32,
}

impl Default for LightmapAllocator {
    fn default() -> LightmapAllocator {
        LightmapAllocator::new()
    }
}

impl LightmapAllocator {
    /// `BeginLightmapAllocation` (`cmatlightmaps.cpp:279`), with page 0
    /// reserved for [`WHITE_PAGE`].
    pub fn new() -> LightmapAllocator {
        LightmapAllocator {
            open: vec![(1, ImagePacker::new(PAGE_WIDTH, PAGE_HEIGHT))],
            page_count: 2,
        }
    }

    /// Starts a new material's run of surfaces.
    ///
    /// Closes every page but the most recent, per `AllocateLightmap`'s
    /// material-change branch.
    pub fn begin_material(&mut self) {
        if self.open.len() > 1 {
            let last = self.open.pop().expect("checked non-empty");
            self.open.clear();
            self.open.push(last);
        }
    }

    /// Places a `width` x `height` block, opening a new page if it does not
    /// fit in any open one.
    ///
    /// `width` is already multiplied by [`BUMP_BLOCKS`] for a bumped surface,
    /// as `RegisterLightmappedSurface` multiplies it.
    ///
    /// Returns `None` only for a block that cannot fit an empty page at all,
    /// where the original called `Error()` and killed the process. A `.bsp` is
    /// untrusted input (`rustdocs/ENGINE.md` gotcha #7), so an impossible
    /// lightmap size becomes a white lightmap and a warning instead.
    pub fn allocate(&mut self, width: u32, height: u32) -> Option<Allocation> {
        for (page, packer) in &mut self.open {
            if let Some((x, y)) = packer.add_block(width, height) {
                return Some(Allocation { page: *page, x, y });
            }
        }

        let page = self.page_count;
        let mut packer = ImagePacker::new(PAGE_WIDTH, PAGE_HEIGHT);
        let (x, y) = packer.add_block(width, height)?;
        self.page_count += 1;
        self.open.push((page, packer));
        Some(Allocation { page, x, y })
    }

    /// How many pages exist, [`WHITE_PAGE`] included.
    pub fn page_count(&self) -> u32 {
        self.page_count
    }
}

/// The lightmap atlas: CPU-side pixels while a map loads, GPU textures after.
///
/// Separated from [`LightmapAllocator`] because they have different lifetimes
/// and only one of them needs a device: allocation and writing are pure CPU
/// work that `src/engine/world/` can be tested against, and
/// [`upload`](LightmapAtlas::upload) is the single point that touches `wgpu`.
pub struct LightmapAtlas {
    allocator: LightmapAllocator,
    /// One `Rgba16Float` image per page, indexed by page. Page 0 is 1x1 white.
    pages: Vec<Vec<u16>>,
}

impl Default for LightmapAtlas {
    fn default() -> LightmapAtlas {
        LightmapAtlas::new()
    }
}

impl LightmapAtlas {
    pub fn new() -> LightmapAtlas {
        LightmapAtlas {
            allocator: LightmapAllocator::new(),
            // Page 0: one opaque white texel. `TEXTURE_LIGHTMAP_WHITE`.
            pages: vec![vec![ONE_F16; 4]],
        }
    }

    /// See [`LightmapAllocator::begin_material`].
    pub fn begin_material(&mut self) {
        self.allocator.begin_material();
    }

    /// Places one surface's lightmap and returns where it went.
    pub fn allocate(&mut self, width: u32, height: u32) -> Option<Allocation> {
        let allocation = self.allocator.allocate(width, height)?;
        // Pages are created lazily here rather than in the allocator so that
        // the allocator stays free of the pixel storage it does not need.
        while self.pages.len() < self.allocator.page_count() as usize {
            self.pages
                .push(vec![0; (PAGE_WIDTH * PAGE_HEIGHT * 4) as usize]);
        }
        Some(allocation)
    }

    /// The dimensions of a page, for the texture-coordinate scale.
    ///
    /// `CMatLightmaps::GetLightmapPageSize` (`cmatlightmaps.cpp:148`).
    /// **Every page is full size here**, including the last: Valve shrank the
    /// final page to `GetMinimumDimensions`' power-of-two height
    /// (`imagepacker.cpp:156`) to save video memory on a fixed console budget,
    /// at the cost of making every surface's texture coordinates depend on
    /// which page it landed on and how full that page ended up. The saving is
    /// at most one page; the risk is a whole class of coordinate bug. Reverse
    /// it here if it is ever worth it.
    pub fn page_size(&self, page: u32) -> (u32, u32) {
        if page == WHITE_PAGE {
            (1, 1)
        } else {
            (PAGE_WIDTH, PAGE_HEIGHT)
        }
    }

    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    /// Writes one surface's light samples into its page.
    ///
    /// `samples` is the `.bsp`'s own bytes for lightstyle 0: `source_blocks *
    /// width * height` entries, the flat map first and then each bump-basis
    /// map, where `source_blocks` is whatever [`surf::BUMPLIGHT`] said. It is
    /// decoded here rather than by the caller so that the whole
    /// `ColorRGBExp32` -> page-format path is one function.
    ///
    /// `blocks` is what the *material* asked for, which is not always what the
    /// file holds — see [`the mismatch`](LightmapAtlas#a-material-and-a-face-can-disagree)
    /// below. `allocation` must have been made `blocks * width` wide.
    ///
    /// This is `BumpedLightmapBitsToPixelWriter_HDRF` /
    /// `LightmapBitsToPixelWriter_HDRF` (`cmatlightmaps.cpp:936`, `:1826`),
    /// including the correction the first of those carries a note about:
    /// *"[mariod] - LinearToBumpedLightmap() was entirely missing in the float
    /// path as of September '11"*. It is applied.
    ///
    /// <a id="a-material-and-a-face-can-disagree"></a>
    /// # A material and a face can disagree
    ///
    /// How wide a block to *reserve* comes from the material — that is Valve's
    /// rule (`RegisterLightmappedSurface`, `gl_matsysiface.cpp:216`) and
    /// keeping it is what keeps every surface of one material sampling the
    /// same way. How many blocks the file *holds* comes from
    /// [`surf::BUMPLIGHT`]. The two agree on shipped content and can disagree
    /// if a `.vmt` gained or lost a `$bumpmap` after the map was compiled, so:
    ///
    /// - the material wants four and the file has one: the flat map is copied
    ///   into all four blocks, which is what a flat normal would sample anyway;
    /// - the material wants one and the file has four: only the flat map is
    ///   written and the rest is ignored.
    ///
    /// Valve's engine has no such reconciliation — it reads the lump at the
    /// material's stride and walks off the end of the face.
    pub fn write(
        &mut self,
        allocation: Allocation,
        width: u32,
        height: u32,
        blocks: u32,
        samples: &[ColorRgbExp32],
    ) {
        let texels = (width * height) as usize;
        if texels == 0 || samples.len() < texels {
            return;
        }
        let source_blocks = (samples.len() / texels).min(BUMP_BLOCKS as usize);
        let (page_width, _) = self.page_size(allocation.page);
        let page = &mut self.pages[allocation.page as usize];

        for t in 0..height {
            for s in 0..width {
                let source = (t * width + s) as usize;
                let flat = samples[source].to_linear();
                let mut color = [flat; BUMP_BLOCKS as usize];
                if source_blocks == BUMP_BLOCKS as usize {
                    linear_to_bumped_lightmap(
                        flat,
                        samples[texels + source].to_linear(),
                        samples[2 * texels + source].to_linear(),
                        samples[3 * texels + source].to_linear(),
                        &mut color,
                    );
                }

                for block in 0..blocks {
                    let x = allocation.x + block * width + s;
                    let y = allocation.y + t;
                    let at = ((y * page_width + x) * 4) as usize;
                    let rgb = color[block as usize];
                    page[at] = f32_to_f16(rgb[0]);
                    page[at + 1] = f32_to_f16(rgb[1]);
                    page[at + 2] = f32_to_f16(rgb[2]);
                    // Valve's alpha is the CSM shadow term, written only when
                    // `GetCSMAccurateBlending()`; cascaded shadow maps are not
                    // ported, so it is the opaque 1.0 the sampler would want.
                    page[at + 3] = ONE_F16;
                }
            }
        }
    }

    /// Uploads every page and builds the bind group each one is drawn with.
    ///
    /// One `wgpu::Texture` per page, no mips —
    /// `AllocateLightmapTexture` (`cmatlightmaps.cpp:456`) says "don't mipmap
    /// lightmaps", and a mipped atlas would bleed one surface's light into its
    /// neighbour's.
    pub fn upload(
        self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layouts: &BindLayouts,
    ) -> LightmapPages {
        let sampler = device.create_sampler(&lightmap_sampler());
        let pages = self
            .pages
            .into_iter()
            .enumerate()
            .map(|(index, pixels)| {
                let page = index as u32;
                let (width, height) = if page == WHITE_PAGE {
                    (1, 1)
                } else {
                    (PAGE_WIDTH, PAGE_HEIGHT)
                };
                let texture = Arc::new(Texture::from_pixels(
                    device,
                    queue,
                    &format!("[lightmap {page}]"),
                    width,
                    height,
                    wgpu::TextureFormat::Rgba16Float,
                    bytemuck::cast_slice(&pixels),
                    sampler.clone(),
                ));
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("lightmap page"),
                    layout: layouts.lightmap(),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(texture.view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(texture.sampler()),
                        },
                    ],
                });
                LightmapPage {
                    texture,
                    bind_group,
                }
            })
            .collect();
        LightmapPages { pages }
    }
}

/// One uploaded atlas page.
pub struct LightmapPage {
    /// Held so the view the bind group refers to stays alive.
    #[allow(dead_code)]
    texture: Arc<Texture>,
    bind_group: wgpu::BindGroup,
}

impl LightmapPage {
    /// Bind group 3, as [`Pass::bind_lightmap_page`](super::context::Pass::bind_lightmap_page)
    /// wants it.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

/// Every page of one map's atlas, on the GPU.
pub struct LightmapPages {
    pages: Vec<LightmapPage>,
}

impl LightmapPages {
    /// The page a batch names, or the white page if it names one that does not
    /// exist.
    pub fn page(&self, page: u32) -> &LightmapPage {
        self.pages
            .get(page as usize)
            .unwrap_or(&self.pages[WHITE_PAGE as usize])
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Bytes of video memory the atlas occupies. Reported in the map summary,
    /// which is where anyone would look on being surprised by it.
    pub fn bytes(&self) -> usize {
        // Page 0 is 1x1; the rest are full size. 8 bytes a texel.
        8 + (self.pages.len().saturating_sub(1)) * (PAGE_WIDTH * PAGE_HEIGHT * 4 * 2) as usize
    }
}

/// The sampler every lightmap page shares.
///
/// `AllocateLightmapTexture` sets `SHADER_TEXFILTERMODE_LINEAR` for both min
/// and mag and creates the texture with one mip. Clamping is not stated there
/// because a page is never sampled outside `[0,1]` — but a surface at the very
/// edge of a page is one bilinear tap away from wrapping onto the opposite
/// side, so it is stated here.
fn lightmap_sampler() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("lightmap"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
    }
}

/// Rescales the three bump-basis maps so their average matches the flat one.
///
/// `ColorSpace::LinearToBumpedLightmap`'s float overload
/// (`materialsystem/colorspace.h:330`). Valve's comment says what it is for:
/// *"find a scale factor which makes the average of the 3 bumped mapped
/// vectors match the straight up vector (if possible), so that flat bumpmapped
/// areas match non-bumpmapped areas"* — without it a wall with a normal map
/// and a wall without one, lit identically, come out different brightnesses.
///
/// The comment two lines further down is also Valve's and is worth keeping:
/// *"Note: According to Alex, this code is completely wrong. Because the bump
/// vectors constitute a orthonormal basis, one does not simply average them
/// [...] they are added together then multiplied by 0.575"*. It is reproduced
/// as written, because the shipped lightmaps were baked against it.
///
/// The flat map passes through unchanged, and a channel whose bump average is
/// exactly zero gets a zero scale rather than a division.
fn linear_to_bumped_lightmap(
    flat: [f32; 3],
    bump1: [f32; 3],
    bump2: [f32; 3],
    bump3: [f32; 3],
    out: &mut [[f32; 3]; BUMP_BLOCKS as usize],
) {
    let mut scale = [0.0f32; 3];
    for channel in 0..3 {
        let average = (bump1[channel] + bump2[channel] + bump3[channel]) / 3.0;
        scale[channel] = if average != 0.0 {
            flat[channel] / average
        } else {
            0.0
        };
    }
    out[0] = flat;
    for channel in 0..3 {
        out[1][channel] = bump1[channel] * scale[channel];
        out[2][channel] = bump2[channel] * scale[channel];
        out[3][channel] = bump3[channel] * scale[channel];
    }
}

/// `1.0` as IEEE binary16 bits.
const ONE_F16: u16 = 0x3C00;

/// The largest finite binary16 value.
const MAX_F16: f32 = 65504.0;

/// Encodes a non-negative `f32` as IEEE binary16 bits.
///
/// Rust has no `f16`, `wgpu` wants the bits, and the one crate that would
/// provide it (`half`) is not worth a dependency for twenty lines —
/// `Cargo.toml`'s rule is that a dependency has to replace more than it costs.
///
/// Lightmap radiance is non-negative and finite, so the negative and NaN cases
/// collapse to zero rather than being encoded: a negative luxel is corrupt
/// data, and drawing it as black is what `Assert( blocklights[...] >= 0.0f )`
/// (`gl_lightmap.cpp:698`) checked for. Rounding is half-up rather than
/// half-to-even; the difference is one unit in the last of ten mantissa bits.
fn f32_to_f16(value: f32) -> u16 {
    // NaN as well as zero and negatives: a corrupt luxel draws black rather
    // than propagating through the exponent arithmetic below.
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= MAX_F16 {
        return 0x7BFF;
    }

    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xFF) as i32 - 127;
    let mantissa = bits & 0x007F_FFFF;

    if exponent >= -14 {
        // Normal. The carry out of a rounded-up mantissa lands in the
        // exponent field, which is exactly right.
        let half = (((exponent + 15) as u32) << 10) | (mantissa >> 13);
        return (half + ((mantissa >> 12) & 1)) as u16;
    }
    if exponent < -25 {
        return 0;
    }
    // Subnormal: the implicit leading 1 becomes explicit and shifts out.
    let implicit = mantissa | 0x0080_0000;
    let shift = (-14 - exponent) as u32 + 13;
    ((implicit >> shift) + ((implicit >> (shift - 1)) & 1)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey sample whose linear value is `value / 255`.
    fn sample(value: u8) -> ColorRgbExp32 {
        ColorRgbExp32 {
            r: value,
            g: value,
            b: value,
            exponent: 0,
        }
    }

    /// The mantissa is a fraction of 255, not a count. Getting this wrong is
    /// the difference between a lit room and a white screen, and it does not
    /// look like a decoding bug from either end — the samples are plausible
    /// and the picture is uniformly saturated.
    #[test]
    fn a_full_mantissa_at_exponent_zero_is_one() {
        let white = ColorRgbExp32 {
            r: 255,
            g: 255,
            b: 255,
            exponent: 0,
        };
        assert_eq!(white.to_linear(), [1.0, 1.0, 1.0]);

        let color = ColorRgbExp32 {
            r: 51,
            g: 102,
            b: 204,
            exponent: 0,
        };
        let linear = color.to_linear();
        assert!((linear[0] - 0.2).abs() < 1e-6);
        assert!((linear[1] - 0.4).abs() < 1e-6);
        assert!((linear[2] - 0.8).abs() < 1e-6);
    }

    /// And the exponent is the HDR half: `vrad` uses it to carry a bounce
    /// solution well past 1.0, which is why the page is a float format.
    #[test]
    fn the_exponent_scales_by_powers_of_two() {
        let at = |exponent| {
            ColorRgbExp32 {
                r: 255,
                g: 255,
                b: 255,
                exponent,
            }
            .to_linear()[0]
        };
        assert_eq!(at(0), 1.0);
        assert_eq!(at(4), 16.0, "the top of the domain the original names");
        assert_eq!(at(-2), 0.25);
    }

    #[test]
    fn a_lightmap_sample_is_four_bytes() {
        assert_eq!(size_of::<ColorRgbExp32>(), 4);
    }

    #[test]
    fn f16_encodes_the_values_a_lightmap_actually_holds() {
        assert_eq!(f32_to_f16(0.0), 0);
        assert_eq!(f32_to_f16(1.0), ONE_F16);
        assert_eq!(f32_to_f16(2.0), 0x4000);
        assert_eq!(f32_to_f16(0.5), 0x3800);
        // 1/1024 is the smallest normal half; below it the encoding is
        // subnormal, which the measured lightmap data reaches often — 96% of
        // `sp_a1_intro1`'s luxels are under 0.25.
        assert_eq!(f32_to_f16(f32::exp2(-14.0)), 0x0400);
        assert_eq!(f32_to_f16(f32::exp2(-15.0)), 0x0200);
        assert_eq!(f32_to_f16(f32::exp2(-24.0)), 0x0001);
        assert_eq!(f32_to_f16(f32::exp2(-30.0)), 0);
    }

    #[test]
    fn f16_clamps_rather_than_producing_infinities() {
        assert_eq!(f32_to_f16(1.0e30), 0x7BFF);
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7BFF);
        assert_eq!(f32_to_f16(f32::NAN), 0);
        assert_eq!(f32_to_f16(-1.0), 0);
    }

    /// Round-trips through the encoding at the magnitudes lightmaps use, since
    /// the failure mode of a wrong exponent is a picture that is uniformly too
    /// dark or too bright rather than anything that errors.
    #[test]
    fn f16_round_trips_within_half_precision() {
        for value in [0.03125, 0.1, 0.25, 0.5, 1.0, 1.32, 4.0, 16.0] {
            let bits = f32_to_f16(value);
            let decoded = decode_f16(bits);
            assert!(
                (decoded - value).abs() <= value * 0.001,
                "{value} encoded to {bits:#06x}, decoded to {decoded}"
            );
        }
    }

    /// The inverse of [`f32_to_f16`], for tests only.
    fn decode_f16(bits: u16) -> f32 {
        let exponent = ((bits >> 10) & 0x1F) as i32;
        let mantissa = (bits & 0x3FF) as f32;
        if exponent == 0 {
            mantissa * f32::exp2(-24.0)
        } else {
            (1.0 + mantissa / 1024.0) * f32::exp2((exponent - 15) as f32)
        }
    }

    #[test]
    fn the_packer_fills_a_row_left_to_right() {
        let mut packer = ImagePacker::new(64, 64);
        assert_eq!(packer.add_block(16, 8), Some((0, 0)));
        assert_eq!(packer.add_block(16, 8), Some((16, 0)));
        assert_eq!(packer.add_block(16, 8), Some((32, 0)));
    }

    /// The skyline: a block that does not fit beside the tall one goes above
    /// it only where the wavefront allows, not at a global top.
    #[test]
    fn the_packer_stacks_on_the_wavefront() {
        let mut packer = ImagePacker::new(64, 64);
        assert_eq!(packer.add_block(32, 16), Some((0, 0)));
        assert_eq!(packer.add_block(32, 4), Some((32, 0)));
        // Fits beside the short block, at its top, not above the tall one.
        assert_eq!(packer.add_block(16, 4), Some((32, 4)));
    }

    /// `AddBlock`'s height test is `>=` against `height - 1`, so a page is
    /// full one row early. Reproduced from the original; a port that used `>`
    /// would place blocks the original spilled onto the next page, and every
    /// following surface's texture coordinates would move.
    #[test]
    fn the_packer_reserves_valves_last_row() {
        let mut packer = ImagePacker::new(16, 16);
        assert_eq!(packer.add_block(16, 14), Some((0, 0)));
        assert_eq!(packer.add_block(16, 1), None);
    }

    #[test]
    fn the_packer_refuses_a_block_wider_than_the_page() {
        let mut packer = ImagePacker::new(16, 16);
        assert_eq!(packer.add_block(17, 1), None);
        assert_eq!(packer.add_block(8, 8), Some((0, 0)));
    }

    #[test]
    fn a_block_that_does_not_fit_opens_a_new_page() {
        let mut atlas = LightmapAtlas::new();
        let first = atlas.allocate(PAGE_WIDTH, PAGE_HEIGHT / 2).expect("fits");
        let second = atlas.allocate(PAGE_WIDTH, PAGE_HEIGHT / 2).expect("fits");
        assert_eq!(first.page, 1);
        assert_eq!(second.page, 2);
        // Page 0 is the white page, so two full pages means three in total.
        assert_eq!(atlas.page_count(), 3);
    }

    /// `AllocateLightmap`'s material-change branch: every page but the last is
    /// closed, so a later material cannot reopen an early page and split a
    /// batch that could have been one.
    #[test]
    fn a_new_material_closes_every_page_but_the_last() {
        let mut allocator = LightmapAllocator::new();
        // Fill page 1 and open page 2 with room to spare in both.
        let a = allocator.allocate(PAGE_WIDTH, 200).expect("fits");
        let b = allocator.allocate(PAGE_WIDTH, 200).expect("fits");
        assert_eq!((a.page, b.page), (1, 2));

        allocator.begin_material();
        // Page 1 still has room, but it is closed: this lands on page 2.
        let c = allocator.allocate(16, 16).expect("fits");
        assert_eq!(c.page, 2);
    }

    #[test]
    fn the_white_page_is_one_texel_and_page_zero() {
        let atlas = LightmapAtlas::new();
        assert_eq!(WHITE_PAGE, 0);
        assert_eq!(atlas.page_size(WHITE_PAGE), (1, 1));
        assert_eq!(atlas.pages[0], vec![ONE_F16; 4]);
    }

    #[test]
    fn an_unbumped_block_is_written_where_it_was_allocated() {
        let mut atlas = LightmapAtlas::new();
        let allocation = atlas.allocate(2, 2).expect("fits");
        atlas.write(allocation, 2, 2, 1, &[sample(255); 4]);

        let page = &atlas.pages[allocation.page as usize];
        let at = |x: u32, y: u32| ((y * PAGE_WIDTH + x) * 4) as usize;
        for y in 0..2 {
            for x in 0..2 {
                let i = at(allocation.x + x, allocation.y + y);
                assert_eq!(&page[i..i + 3], &[f32_to_f16(1.0); 3]);
                assert_eq!(page[i + 3], ONE_F16, "alpha is opaque");
            }
        }
        // The texel past the block is untouched.
        let outside = at(allocation.x + 2, allocation.y);
        assert_eq!(&page[outside..outside + 4], &[0, 0, 0, 0]);
    }

    /// The three directional maps go to the right of the flat one, in order,
    /// which is what makes the shader's "add the offset once, twice, three
    /// times" reach them.
    #[test]
    fn a_bumped_block_lays_its_four_maps_out_left_to_right() {
        let mut atlas = LightmapAtlas::new();
        let allocation = atlas.allocate(BUMP_BLOCKS, 1).expect("fits");
        // Bump maps that already average to the flat one, so the correction
        // scale is 1 and each block keeps its own value.
        atlas.write(
            allocation,
            1,
            1,
            BUMP_BLOCKS,
            &[sample(3 * 51), sample(51), sample(3 * 51), sample(5 * 51)],
        );

        let page = &atlas.pages[allocation.page as usize];
        let value = |block: u32| {
            let at = (((allocation.y * PAGE_WIDTH) + allocation.x + block) * 4) as usize;
            decode_f16(page[at])
        };
        let unit = 51.0 / 255.0;
        for (block, expected) in [(0, 3.0), (1, 1.0), (2, 3.0), (3, 5.0)] {
            assert!(
                (value(block) - expected * unit).abs() < 1e-3,
                "block {block} is {}, expected {}",
                value(block),
                expected * unit
            );
        }
    }

    /// `LinearToBumpedLightmap`'s whole job: a surface with a flat normal must
    /// come out as bright as the same surface without a normal map.
    #[test]
    fn the_bump_correction_makes_the_three_maps_average_to_the_flat_one() {
        let mut out = [[0.0f32; 3]; 4];
        linear_to_bumped_lightmap(
            [6.0, 6.0, 6.0],
            [1.0, 1.0, 1.0],
            [2.0, 2.0, 2.0],
            [3.0, 3.0, 3.0],
            &mut out,
        );
        assert_eq!(out[0], [6.0, 6.0, 6.0]);
        for channel in 0..3 {
            let average = (out[1][channel] + out[2][channel] + out[3][channel]) / 3.0;
            assert!((average - 6.0).abs() < 1e-5, "{out:?}");
        }
        // The ratios between the three are preserved: it is a scale, not a
        // rewrite.
        assert!((out[2][0] / out[1][0] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn the_bump_correction_does_not_divide_by_a_zero_average() {
        let mut out = [[0.0f32; 3]; 4];
        linear_to_bumped_lightmap(
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            &mut out,
        );
        assert_eq!(out[0], [1.0, 1.0, 1.0]);
        for directional in &out[1..] {
            assert_eq!(*directional, [0.0, 0.0, 0.0]);
        }
    }
}
