//! Counting how bright a rendered frame is: the measurement half of auto
//! exposure.
//!
//! Replaces `CTonemapSystem`'s query machinery —
//! `CHistogramBucket::IssueQuery`, `IssueAndReceiveBucketQueries` and
//! `stdshaders/luminance_compare_ps2x.fxc` (`game/client/viewpostprocess.cpp`
//! :549-830). **Only the machinery**: what the buckets are, what to do with the
//! counts and how fast to react are the tone mapper's, and live in
//! [`client::tonemap`](crate::client::tonemap). This module is told a list of
//! luminance boundaries and answers with a pixel count per bucket.
//!
//! # Why this is not a port of the mechanism
//!
//! Valve had no way to read a texture back cheaply, so the histogram was built
//! out of occlusion queries: draw a full-screen rectangle with a shader that
//! kills every pixel outside one luminance range, and ask the hardware how many
//! survived. One query per frame (`MAX_QUERIES_PER_FRAME` is 1), sixteen
//! buckets, so a complete histogram was **sixteen frames old** and the bins
//! were rebuilt in a rolling wave — which is what
//! `SetTonemapScale`'s comment about "riding the wave of the histogram
//! re-building" is guarding against. A compute pass bins every pixel into every
//! bucket in one dispatch, so the whole histogram is two frames old and none of
//! it is stale relative to the rest. `PORTING.md`'s rule applies directly: the
//! question is domain knowledge, the occlusion queries were an encoding.
//!
//! # The latency, and why the readback is never waited on
//!
//! [`record`](Histogram::record) queues the dispatch and a copy into a staging
//! buffer; [`take`](Histogram::take) picks up whatever has finished. Nothing
//! blocks: a frame with no result ready is a frame the tone mapper does not
//! adjust on, which is the same thing Valve's fifteen-out-of-sixteen frames
//! were. Blocking would hand the GPU's pipeline depth back in exchange for a
//! number that is about to be smoothed over a second anyway.
//!
//! **[`take`] is what arms the readback**, one call after the record. That is
//! not a style choice: `map_async` on a buffer whose copy has been *recorded*
//! but not *submitted* would map it immediately, and the submit would then be a
//! validation error for writing to a mapped buffer. Deferring to the next call
//! means the frame it was recorded into has been presented.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

/// The most buckets one dispatch can bin into.
///
/// The size of the per-workgroup scratch array in `shaders/histogram.wgsl`,
/// which has to be a compile-time constant there. The *useful* count is
/// whatever the caller's boundary list implies, and Valve's is
/// `NUM_HISTOGRAM_BUCKETS_NEW - 1` — sixteen, which is where this number comes
/// from.
pub const MAX_BUCKETS: usize = 16;

/// How many staging buffers the readback rotates through.
///
/// Two is enough to have a measurement ready every frame once the pipeline is
/// full: one is being mapped while the next is being filled. A third would only
/// buy tolerance for a GPU more than two frames behind, and the tone mapper
/// does not care.
const SLOTS: usize = 2;

/// One pixel count per bucket, plus how many pixels were looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// `CHistogramBucket::m_nPixelsInRange`, one per bucket, in ascending
    /// luminance order.
    ///
    /// Only the first `len` entries are meaningful. They sum to the number of
    /// pixels measured, exactly — unlike Valve's, where a pixel sitting on a
    /// bucket boundary was counted by *both* neighbours because each range test
    /// was a pair of inclusive `step()`s. See [`Histogram`]'s divergence note.
    pub buckets: [u32; MAX_BUCKETS],
    /// How many of `buckets` the dispatch that produced this filled.
    pub len: usize,
}

impl Counts {
    /// The buckets that were actually measured.
    pub fn as_slice(&self) -> &[u32] {
        &self.buckets[..self.len]
    }

    /// How many pixels went into this measurement.
    // Read only by the tests, which is where the half-open-bucket divergence
    // above is actually checked; kept because "the buckets sum to the pixel
    // count" is the property that makes it true.
    #[allow(dead_code)]
    pub fn total(&self) -> u32 {
        self.as_slice().iter().sum()
    }
}

/// The rectangle of the source a measurement covers, in texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// The centre `fraction_x` by `fraction_y` of a `width` by `height` image.
    ///
    /// `mat_exposure_center_region_x`/`_y` (`viewpostprocess.cpp:126`), which
    /// `CHistogramBucket::IssueQuery` applies as a border of
    /// `0.5 * (1 - fraction)` on each side. Portal 2's defaults are 0.9 and
    /// 0.85, so the corners of the screen — where a wall or the sky usually is
    /// — do not decide the exposure.
    ///
    /// Never empty: a fraction small enough to round the rectangle away still
    /// yields one texel, because a measurement of nothing is worse than a
    /// measurement of one pixel.
    pub fn centered(width: u32, height: u32, fraction_x: f32, fraction_y: f32) -> Region {
        let inset = |size: u32, fraction: f32| -> (u32, u32) {
            let border = (size as f32 * 0.5 * (1.0 - fraction.clamp(0.0, 1.0))) as u32;
            let border = border.min(size.saturating_sub(1) / 2);
            (border, size - 2 * border)
        };
        let (x, width) = inset(width.max(1), fraction_x);
        let (y, height) = inset(height.max(1), fraction_y);
        Region {
            x,
            y,
            width,
            height,
        }
    }
}

/// What `shaders/histogram.wgsl`'s `Params` block holds.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Params {
    rect: [u32; 4],
    count: u32,
    pad: [u32; 3],
}

/// Where one staging buffer is in the record → map → read cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// Nothing owns it.
    Free,
    /// A copy into it has been recorded, and may not have been submitted yet.
    Recorded,
    /// `map_async` has been called; the callback has not been seen.
    Mapping,
}

/// Set by the `map_async` callback, which runs on whichever thread drives the
/// poll. 0 pending, 1 mapped, 2 failed.
const MAP_PENDING: u8 = 0;
const MAP_READY: u8 = 1;
const MAP_FAILED: u8 = 2;

struct Slot {
    buffer: wgpu::Buffer,
    state: SlotState,
    status: Arc<AtomicU8>,
    /// Which record filled it, so that two results arriving together resolve in
    /// order rather than by slot index.
    sequence: u64,
}

/// A luminance histogram of a texture, measured on the GPU.
///
/// # One deliberate divergence from Valve
///
/// **Bucket ranges here are half-open and Valve's were closed.**
/// `luminance_compare_ps2x.fxc` tests `step(min, l) * step(l, max)`, so a pixel
/// whose luminance lands exactly on a boundary is counted by the bucket below
/// *and* the bucket above, and the bucket totals therefore do not sum to the
/// pixel count. With an 8-bit frame buffer that is not a corner case — large
/// flat areas quantize to the same value, and a boundary landing on one of the
/// 256 representable levels double-counts all of it. Here a pixel lands in
/// exactly one bucket, and [`Counts::total`] is the pixel count. Nothing
/// downstream depends on the double counting: `FindLocationOfPercentBrightPixels`
/// works in fractions of the total it computes itself.
pub struct Histogram {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// The boundaries, uploaded once. `count + 1` of them.
    bounds: wgpu::Buffer,
    /// `Params`, rewritten whenever the region changes.
    params: wgpu::Buffer,
    /// Where the dispatch accumulates. Cleared before every dispatch.
    counts: wgpu::Buffer,
    slots: Vec<Slot>,
    count: usize,
    /// The bind group over whatever [`set_source`](Histogram::set_source) was
    /// last given. `None` until it is called, which is what makes
    /// [`record`](Histogram::record) a no-op before there is anything to
    /// measure.
    bind_group: Option<wgpu::BindGroup>,
    /// What was last written into [`Histogram::params`], so that an unchanged
    /// region does not restage the block every frame.
    last_region: Option<Region>,
    sequence: u64,
}

impl Histogram {
    /// Builds the measurement for a fixed set of bucket boundaries.
    ///
    /// `bounds` is ascending, and bucket `i` covers `[bounds[i], bounds[i+1])`
    /// — so there is one more boundary than there are buckets. The first and
    /// last are widened by the shader rather than by the caller: everything
    /// darker than `bounds[1]` lands in bucket 0 and everything from
    /// `bounds[len - 2]` up lands in the last, which is what Valve's `-1e20`
    /// and `+1e20` special cases did.
    ///
    /// # Panics
    ///
    /// If `bounds` describes fewer than one or more than [`MAX_BUCKETS`]
    /// buckets. Both are programming errors in the caller's bucket table, not
    /// conditions to survive at runtime.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, bounds: &[f32]) -> Histogram {
        let count = bounds.len().saturating_sub(1);
        assert!(
            (1..=MAX_BUCKETS).contains(&count),
            "histogram wants 1..={MAX_BUCKETS} buckets, got {count}"
        );

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("histogram"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        // `textureLoad`, so nothing filters and the source may
                        // be any float format — including the sRGB back-buffer
                        // format, which decodes on fetch.
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("histogram"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/histogram.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("histogram"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("histogram"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let bounds_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram bounds"),
            size: std::mem::size_of_val(bounds) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&bounds_buffer, 0, bytemuck::cast_slice(bounds));

        let counts_size = (count * size_of::<u32>()) as u64;
        let slots = (0..SLOTS)
            .map(|i| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("histogram readback {i}")),
                    size: counts_size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: SlotState::Free,
                status: Arc::new(AtomicU8::new(MAP_PENDING)),
                sequence: 0,
            })
            .collect();

        Histogram {
            pipeline,
            layout,
            bounds: bounds_buffer,
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("histogram params"),
                size: size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            counts: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("histogram counts"),
                size: counts_size,
                // `COPY_DST` is for `clear_buffer`, which counts as a write.
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            slots,
            count,
            bind_group: None,
            last_region: None,
            device: device.clone(),
            queue: queue.clone(),
            sequence: 0,
        }
    }

    /// How many buckets a measurement comes back with.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Points the measurement at a texture.
    ///
    /// Call whenever the thing being measured is created or reallocated — a
    /// window resize, for this port's one caller. A `wgpu::TextureView` carries
    /// no identity a caller could compare, so this is explicit rather than
    /// detected: a `record` against a stale bind group is a measurement of a
    /// texture that no longer exists, which `wgpu` reports as a validation
    /// error rather than a wrong number, but only because the old texture is
    /// still alive inside the bind group.
    ///
    /// `source` must be a 2D view of a texture with `TEXTURE_BINDING` usage.
    pub fn set_source(&mut self, source: &wgpu::TextureView) {
        self.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("histogram"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.bounds.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.counts.as_entire_binding(),
                },
            ],
        }));
    }

    /// Records a measurement of the source over `region` into `encoder`.
    ///
    /// Does nothing, and reports `false`, if there is no source, the region is
    /// empty, or every staging buffer is still busy. None of those is a
    /// failure: the previous measurement simply stands for another frame.
    ///
    /// `region` must lie inside the source — the shader clamps nothing, it
    /// skips invocations outside the rectangle, so an oversized region reads
    /// out of bounds. [`Region::centered`] cannot produce one.
    pub fn record(&mut self, encoder: &mut wgpu::CommandEncoder, region: Region) -> bool {
        if region.width == 0 || region.height == 0 || self.bind_group.is_none() {
            return false;
        }
        let Some(slot) = self
            .slots
            .iter()
            .position(|slot| slot.state == SlotState::Free)
        else {
            return false;
        };

        // Restaged only when the region changes, which is on a resize. Here
        // rather than in the caller because a stale block is a measurement of
        // the wrong rectangle rather than an error.
        if self.last_region != Some(region) {
            self.queue.write_buffer(
                &self.params,
                0,
                bytemuck::bytes_of(&Params {
                    rect: [region.x, region.y, region.width, region.height],
                    count: self.count as u32,
                    pad: [0; 3],
                }),
            );
            self.last_region = Some(region);
        }
        let bind_group = self.bind_group.as_ref().expect("checked above");

        // Accumulating into a buffer means starting from zero every time.
        encoder.clear_buffer(&self.counts, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("histogram"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(region.width.div_ceil(8), region.height.div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.counts,
            0,
            &self.slots[slot].buffer,
            0,
            self.counts.size(),
        );

        self.sequence += 1;
        self.slots[slot].sequence = self.sequence;
        self.slots[slot].state = SlotState::Recorded;
        true
    }

    /// The newest measurement that has come back, if any.
    ///
    /// Call once a frame, **before** [`record`](Histogram::record): it is also
    /// what arms the readback of whatever the previous frame recorded, and that
    /// is only safe once that frame has been submitted. See the module docs.
    pub fn take(&mut self) -> Option<Counts> {
        // Drives the `map_async` callbacks. Non-blocking; a slot that is not
        // ready this frame is ready the next one.
        let _ = self.device.poll(wgpu::PollType::Poll);

        let mut newest: Option<(u64, Counts)> = None;
        for slot in &mut self.slots {
            if slot.state != SlotState::Mapping {
                continue;
            }
            match slot.status.swap(MAP_PENDING, Ordering::Acquire) {
                MAP_READY => {
                    let counts = slot.buffer.slice(..).get_mapped_range().ok().map(|bytes| {
                        let mut buckets = [0u32; MAX_BUCKETS];
                        let read: &[u32] = bytemuck::cast_slice(&bytes);
                        buckets[..self.count].copy_from_slice(&read[..self.count]);
                        Counts {
                            buckets,
                            len: self.count,
                        }
                    });
                    slot.buffer.unmap();
                    slot.state = SlotState::Free;
                    if let Some(counts) = counts {
                        if newest.is_none_or(|(sequence, _)| slot.sequence > sequence) {
                            newest = Some((slot.sequence, counts));
                        }
                    }
                }
                // The buffer never became mapped, so there is nothing to
                // unmap; give the slot back and let the next frame try again.
                MAP_FAILED => slot.state = SlotState::Free,
                _ => {}
            }
        }

        for slot in &mut self.slots {
            if slot.state != SlotState::Recorded {
                continue;
            }
            let status = Arc::clone(&slot.status);
            slot.buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    status.store(
                        match result {
                            Ok(()) => MAP_READY,
                            Err(_) => MAP_FAILED,
                        },
                        Ordering::Release,
                    );
                });
            slot.state = SlotState::Mapping;
        }

        newest.map(|(_, counts)| counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::texture::{SamplerKey, Texture};

    /// The tone mapper's real distribution, `pow( i / 16, 2.5 )`, rebuilt here
    /// rather than imported: this module is meant to work for any ascending
    /// list, and a test that reached into `client/` for one would quietly make
    /// that untrue.
    fn bounds() -> [f32; MAX_BUCKETS + 1] {
        let mut bounds = [0.0f32; MAX_BUCKETS + 1];
        for (i, bound) in bounds.iter_mut().enumerate() {
            *bound = (i as f32 / MAX_BUCKETS as f32).powf(2.5);
        }
        bounds
    }

    /// A device, or `None` if this machine cannot give us one. Same shape as
    /// `preview.rs`'s, and skipped the same way.
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = match pollster::block_on(instance.request_adapter(&Default::default())) {
            Ok(adapter) => adapter,
            Err(err) => {
                eprintln!("skipping: no usable GPU adapter: {err}");
                return None;
            }
        };
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// A `size` x `size` texture of one repeated pixel.
    fn flat(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        size: u32,
        pixel: [u8; 4],
    ) -> Texture {
        let pixels: Vec<u8> = pixel
            .iter()
            .copied()
            .cycle()
            .take((size * size * 4) as usize)
            .collect();
        Texture::from_pixels(
            device,
            queue,
            "histogram source",
            size,
            size,
            format,
            &pixels,
            device.create_sampler(&SamplerKey::simple().descriptor()),
        )
    }

    /// Records one measurement and waits for it, the way the frame loop never
    /// does. Each `take` polls; the loop is what turns the non-blocking API
    /// into a blocking one for a test's benefit.
    fn measure(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        histogram: &mut Histogram,
        region: Region,
    ) -> Counts {
        let mut encoder = device.create_command_encoder(&Default::default());
        assert!(histogram.record(&mut encoder, region), "nothing recorded");
        queue.submit([encoder.finish()]);
        for _ in 0..1000 {
            if let Some(counts) = histogram.take() {
                return counts;
            }
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
        panic!("the readback never completed");
    }

    /// Which bucket a luminance falls in, by the same rule the shader uses.
    fn bucket_of(luminance: f32) -> usize {
        let bounds = bounds();
        (1..MAX_BUCKETS)
            .rev()
            .find(|&i| luminance >= bounds[i])
            .unwrap_or(0)
    }

    #[test]
    fn every_pixel_lands_in_exactly_one_bucket() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut histogram = Histogram::new(&device, &queue, &bounds());
        assert_eq!(histogram.len(), MAX_BUCKETS);

        // Linear format, so the byte *is* the value: 0.8 in every channel, and
        // the NTSC weights sum to 1, so the luminance is 0.8 too.
        let source = flat(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            32,
            [204, 204, 204, 255],
        );
        histogram.set_source(source.view());
        let counts = measure(
            &device,
            &queue,
            &mut histogram,
            Region::centered(32, 32, 1.0, 1.0),
        );

        // Exactly the pixel count, not more: Valve's closed ranges would
        // double-count a boundary, and these are half-open.
        assert_eq!(counts.total(), 32 * 32);
        let expected = bucket_of(204.0 / 255.0);
        assert_eq!(counts.buckets[expected], 32 * 32, "{:?}", counts.as_slice());
    }

    /// **The assumption the whole exposure loop rests on.**
    ///
    /// `dev/lumcompare.vmt` measures the frame buffer through an sRGB read, so
    /// the luminance the buckets are calibrated against is linear light. This
    /// port gets that for free *if* `textureLoad` on an sRGB format decodes
    /// like `textureSample` does — which the WebGPU specification says it does,
    /// and which is worth one test rather than one comment. Mid-grey is 0.502
    /// encoded and 0.216 linear, six buckets apart, so a wrong answer here is
    /// unmissable rather than subtle.
    #[test]
    fn an_srgb_source_is_measured_in_linear_light() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut histogram = Histogram::new(&device, &queue, &bounds());
        let source = flat(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            32,
            [128, 128, 128, 255],
        );
        histogram.set_source(source.view());
        let counts = measure(
            &device,
            &queue,
            &mut histogram,
            Region::centered(32, 32, 1.0, 1.0),
        );

        let linear = bucket_of(0.215_86);
        let encoded = bucket_of(128.0 / 255.0);
        assert_ne!(linear, encoded, "the test cannot tell the two apart");
        assert_eq!(
            counts.buckets[linear],
            32 * 32,
            "mid-grey was binned as {:?}; linear wants bucket {linear}, \
             a raw sRGB byte would be bucket {encoded}",
            counts.as_slice()
        );
    }

    #[test]
    fn only_the_region_is_measured() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut histogram = Histogram::new(&device, &queue, &bounds());
        let source = flat(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            [255, 255, 255, 255],
        );
        histogram.set_source(source.view());

        let region = Region::centered(64, 64, 0.5, 0.5);
        assert_eq!(region.width * region.height, 32 * 32);
        let counts = measure(&device, &queue, &mut histogram, region);
        assert_eq!(counts.total(), 32 * 32);
        // White: the top bucket, which is also the one that has to catch
        // everything at or above its lower bound.
        assert_eq!(counts.buckets[MAX_BUCKETS - 1], 32 * 32);
    }

    #[test]
    fn black_lands_in_the_first_bucket_and_nothing_is_lost() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut histogram = Histogram::new(&device, &queue, &bounds());
        let source = flat(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            16,
            [0, 0, 0, 255],
        );
        histogram.set_source(source.view());
        let counts = measure(
            &device,
            &queue,
            &mut histogram,
            Region::centered(16, 16, 1.0, 1.0),
        );
        assert_eq!(counts.buckets[0], 16 * 16);
        assert_eq!(counts.total(), 16 * 16);
    }

    #[test]
    fn a_measurement_does_not_accumulate_across_frames() {
        // The counts buffer is cleared before every dispatch; without that,
        // the second measurement would read twice the pixels and the exposure
        // would be computed from a histogram that only ever grows.
        let Some((device, queue)) = device() else {
            return;
        };
        let mut histogram = Histogram::new(&device, &queue, &bounds());
        let source = flat(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            16,
            [255, 255, 255, 255],
        );
        histogram.set_source(source.view());
        let region = Region::centered(16, 16, 1.0, 1.0);
        for _ in 0..4 {
            assert_eq!(
                measure(&device, &queue, &mut histogram, region).total(),
                256
            );
        }
    }

    #[test]
    fn recording_without_a_source_is_a_no_op_rather_than_a_validation_error() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut histogram = Histogram::new(&device, &queue, &bounds());
        let mut encoder = device.create_command_encoder(&Default::default());
        assert!(!histogram.record(&mut encoder, Region::centered(16, 16, 1.0, 1.0)));
        assert!(!histogram.record(
            &mut encoder,
            Region {
                x: 0,
                y: 0,
                width: 0,
                height: 0
            }
        ));
        queue.submit([encoder.finish()]);
        assert_eq!(histogram.take(), None);
    }

    #[test]
    #[should_panic(expected = "histogram wants 1..=16 buckets")]
    fn too_many_buckets_is_a_programming_error() {
        let Some((device, queue)) = device() else {
            // The panic has to happen for the test to pass, so with no device
            // there is nothing honest to do but produce it.
            panic!("histogram wants 1..=16 buckets, got 0 (skipped: no GPU)");
        };
        let bounds = [0.0f32; MAX_BUCKETS + 2];
        let _ = Histogram::new(&device, &queue, &bounds);
    }

    #[test]
    fn centered_region_insets_by_half_the_missing_fraction() {
        // `mat_exposure_center_region_x` 0.9 takes 5% off each side.
        //
        // The vertical border is 74 and not the 75 the arithmetic suggests,
        // because `1 - 0.85` in `f32` is a hair under 0.15 and the border
        // truncates. That is Valve's answer too — `int nBorderHeight = ( ... )
        // * flExposureHeightScale` truncates the same product the same way —
        // and the alternative, rounding, would be a divergence for the sake of
        // one texel.
        let region = Region::centered(1000, 1000, 0.9, 0.85);
        assert_eq!(
            region,
            Region {
                x: 50,
                y: 74,
                width: 900,
                height: 852
            }
        );
    }

    #[test]
    fn centered_region_of_the_whole_image_is_the_whole_image() {
        let region = Region::centered(640, 480, 1.0, 1.0);
        assert_eq!(
            region,
            Region {
                x: 0,
                y: 0,
                width: 640,
                height: 480
            }
        );
    }

    #[test]
    fn centered_region_never_collapses_to_nothing() {
        // A fraction that rounds the rectangle away still leaves a texel:
        // measuring one pixel beats measuring none.
        for size in [1u32, 2, 3, 16] {
            let region = Region::centered(size, size, 0.0, 0.0);
            assert!(
                region.width >= 1 && region.height >= 1,
                "{size}: {region:?}"
            );
            assert!(region.x + region.width <= size, "{size}: {region:?}");
            assert!(region.y + region.height <= size, "{size}: {region:?}");
        }
    }

    #[test]
    fn counts_total_is_the_sum_of_the_measured_buckets() {
        let mut buckets = [7u32; MAX_BUCKETS];
        buckets[4] = 1;
        let counts = Counts { buckets, len: 5 };
        assert_eq!(counts.as_slice(), &[7, 7, 7, 7, 1]);
        assert_eq!(counts.total(), 29);
    }

    #[test]
    fn params_block_is_the_size_the_shader_declares() {
        // `rect` (16) + `count` (4) + three scalars of padding.
        assert_eq!(size_of::<Params>(), 32);
    }
}
