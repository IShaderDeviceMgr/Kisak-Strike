// A luminance histogram of a rendered frame, in one compute dispatch.
//
// Replaces `stdshaders/luminance_compare_ps2x.fxc` and the sixteen occlusion
// queries `CTonemapSystem::IssueAndReceiveBucketQueries` drove it with
// (`game/client/viewpostprocess.cpp:764`). That shader answered one question
// per draw — "is this pixel's luminance between these two numbers?" — and the
// count came back as an occlusion query's pixel count, one bucket per frame.
// The *question* is kept and the mechanism is not: a compute pass bins every
// pixel into every bucket at once, so the whole histogram is a frame old
// rather than sixteen.
//
// The luminance formula is Valve's, unchanged, including the choice of the
// NTSC weights over the two alternatives commented out beside them.

// How many buckets the workgroup reduction has room for. The real count is
// `params.count` and comes from the caller; this is only the size of the
// scratch array, and it is the one number that must match
// `histogram::MAX_BUCKETS` on the Rust side.
const MAX_BUCKETS: u32 = 16u;

// "Formula for calculating luminance based on NTSC standard"
// (`luminance_compare_ps2x.fxc:33`).
const LUMINANCE_WEIGHTS = vec3<f32>(0.2125, 0.7154, 0.0721);

struct Params {
    // The rectangle to measure, in texels of `source`: origin in `xy`, size in
    // `zw`. `mat_exposure_center_region_x`/`_y` inset it; see
    // `CHistogramBucket::IssueQuery`.
    rect: vec4<u32>,
    // How many of `bounds` minus one are real buckets, 1..=MAX_BUCKETS.
    count: u32,
    // A uniform block's size is rounded up to 16 bytes. Spelled out as three
    // scalars rather than a `vec3<u32>`, which would align to 16 and push the
    // block to 48.
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;
// `count + 1` ascending luminance boundaries: bucket `i` is
// `[bounds[i], bounds[i + 1])`. `UpdateBucketRanges`' distribution, computed on
// the CPU because it is the tone mapper's policy and not this shader's.
@group(0) @binding(2) var<storage, read> bounds: array<f32>;
@group(0) @binding(3) var<storage, read_write> counts: array<atomic<u32>>;

// One atomic per bucket per workgroup, so that 64 neighbouring pixels contend
// with each other rather than with the whole dispatch.
var<workgroup> local_counts: array<atomic<u32>, MAX_BUCKETS>;

@compute @workgroup_size(8, 8, 1)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    if lid < MAX_BUCKETS {
        atomicStore(&local_counts[lid], 0u);
    }
    workgroupBarrier();

    // The dispatch is rounded up to whole workgroups, so the tail reads out of
    // the rectangle and must not be counted.
    if gid.x < params.rect.z && gid.y < params.rect.w {
        let texel = vec2<i32>(params.rect.xy + gid.xy);
        // An sRGB format decodes on fetch, so this is linear light — which is
        // what `dev/lumcompare.vmt` measured too, because `screenspace_general`
        // leaves `$LINEARREAD_BASETEXTURE` at 0 and so reads its copy of the
        // frame buffer through an sRGB sampler.
        let color = textureLoad(source, texel, 0).rgb;
        let luminance = dot(color, LUMINANCE_WEIGHTS);

        // Bucket 0 catches everything below `bounds[1]`, including negatives,
        // and the last bucket catches everything from its own lower bound up,
        // including values above 1. That is Valve's `-1e20`/`+1e20` widening of
        // the first and last query ranges, arrived at by construction instead
        // of by special case.
        var bucket = 0u;
        for (var i = 1u; i < params.count; i = i + 1u) {
            if luminance >= bounds[i] {
                bucket = i;
            }
        }
        atomicAdd(&local_counts[bucket], 1u);
    }
    workgroupBarrier();

    if lid < params.count {
        let total = atomicLoad(&local_counts[lid]);
        if total > 0u {
            atomicAdd(&counts[lid], total);
        }
    }
}
