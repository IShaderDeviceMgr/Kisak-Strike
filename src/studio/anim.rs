//! Bones, sequences and the animation that moves them.
//!
//! The `.mdl`'s bone list, sequence table and RLE animation blocks
//! (`public/studio.h`), plus `bonesetup/bone_decode.cpp`'s decoder and the
//! slice of `studiorender/r_studio.cpp`'s `R_StudioSetupBones` that turns a
//! (sequence, cycle) into one matrix per bone.
//!
//! # The animation is decoded at load, not at draw
//!
//! Valve keeps the RLE stream and walks it every frame, because a `.mdl` can
//! carry hundreds of sequences of hundreds of frames and only a handful are
//! ever played. Here every animation is expanded into a plain per-frame table
//! the moment the file is read, and [`pose`] is two array lookups and a blend.
//!
//! That is affordable because it was measured rather than assumed: the four
//! floor-button models the port loads carry **4 or 5 sequences of at most 11
//! frames over 2 to 4 bones**, so a whole model's expanded animation is a few
//! hundred bytes.
//!
//! > **It does not generalise, and the number is known.** The longest
//! > animation in the shipped game is **4,050 frames** — about a megabyte
//! > expanded, for one model — and the game has 5,434 sequences across 2,017
//! > models. Nothing a class loads today is anywhere near that, and the
//! > condition for going back to Valve's lazy walk is the first one that is.
//! > `studio::tests::every_shipped_studio_model_parses` pins both numbers.
//!
//! # What is deliberately absent
//!
//! **Everything that blends.** Valve's `bone_setup.cpp` is ~5,000 lines of
//! layering, pose parameters, IK, procedural bones, bone controllers, blend
//! sequences and interpolation between the sequence you were playing and the
//! one you just switched to. None of it is here: a sequence has one animation
//! (`numblends` 1 for every sequence in every model this loads), a model has no
//! pose parameters, and switching sequences is a cut rather than a cross-fade.
//! `CPropFloorButton` calls `ResetSequence`, which is Valve's own word for a
//! cut.
//!
//! Also absent, each measured: `STUDIO_FRAMEANIM` (the frame-major encoding —
//! no model here uses it), external `.ani` animation blocks, `mstudioanim`'s
//! delta flag, `BONE_FIXED_ALIGNMENT`'s `QuaternionAlign`, IK rules, and
//! zero-frame spans. Each is refused or ignored explicitly rather than
//! silently mis-read; see [`Animation::parse`].

use glam::{Mat4, Quat, Vec3};

use super::mdl::Reader;
use super::StudioError;

// ---------------------------------------------------------------------------
// the file structures
// ---------------------------------------------------------------------------

/// `sizeof(mstudiobone_t)` (`studio.h:404`).
pub(super) const BONE_STRIDE: usize = 216;
/// `sizeof(mstudioseqdesc_t)` (`studio.h:1461`).
pub(super) const SEQUENCE_STRIDE: usize = 212;
/// `sizeof(mstudioanimdesc_t)` (`studio.h:1040`).
pub(super) const ANIM_DESC_STRIDE: usize = 100;

/// `STUDIO_ANIM_*` (`studio.h:866`) — how one bone's channel is stored.
mod anim_flag {
    /// `Vector48`, constant over the timeline.
    pub const RAWPOS: u8 = 0x01;
    /// `Quaternion48`, constant.
    pub const RAWROT: u8 = 0x02;
    /// `mstudioanim_valueptr_t` — RLE per frame.
    pub const ANIMPOS: u8 = 0x04;
    /// `mstudioanim_valueptr_t` — RLE per frame.
    pub const ANIMROT: u8 = 0x08;
    /// The values add to the bind pose rather than replacing it.
    pub const DELTA: u8 = 0x10;
    /// `Quaternion64`, constant.
    pub const RAWROT2: u8 = 0x20;
}

/// `STUDIO_LOOPING` (`studio.h:3577`).
pub const STUDIO_LOOPING: u32 = 0x0001;
/// `STUDIO_ALLZEROS` — the animation has no data at all.
const STUDIO_ALLZEROS: u32 = 0x0020;
/// `STUDIO_FRAMEANIM` — frame-major rather than RLE. Refused; no model the
/// port loads sets it.
const STUDIO_FRAMEANIM: u32 = 0x0040;

/// One bone. `mstudiobone_t` (`studio.h:341`).
#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    /// `parent`, or `None` for a root. Always a *lower* index than this bone's,
    /// which is what lets [`pose`] chain in one forward pass.
    pub parent: Option<usize>,
    /// The bind-pose translation, and what an animated position adds to.
    pub pos: Vec3,
    /// The bind-pose rotation, used when a channel has no rotation at all.
    pub quat: Quat,
    /// `rot` — the bind pose again as a `RadianEuler`, and what an animated
    /// rotation adds to **before** it becomes a quaternion. Not redundant with
    /// [`quat`](Bone::quat): the addition has to happen in Euler space.
    pub rot: Vec3,
    /// `posscale` / `rotscale` — the fixed-point scale each RLE channel's
    /// 16-bit values are multiplied by.
    pub pos_scale: Vec3,
    pub rot_scale: Vec3,
    pub flags: u32,
    /// `poseToBone` — the **inverse bind matrix**, and the reason a posed model
    /// is not inside out. `R_StudioSetupBones` ends in
    /// `ConcatTransforms( boneToWorld[i], poseToBone[i], poseToWorld[i] )`
    /// (`r_studio.cpp:310`), and it is `poseToWorld` that a vertex is
    /// multiplied by. Skip it and every bone applies its own bind transform
    /// twice.
    pub pose_to_bone: Mat4,
}

/// One sequence. `mstudioseqdesc_t` (`studio.h:1331`).
///
/// `LookupSequence( "up" )` matches on [`label`](Sequence::label), case
/// insensitively, which is `Studio_LookupSequence`'s `Q_stricmp`.
#[derive(Debug, Clone)]
pub struct Sequence {
    pub label: String,
    pub flags: u32,
    /// `fadeouttime` — "ideal cross fade out time (0.2 default)", in
    /// **seconds**.
    ///
    /// Nothing here cross-fades, and this is not what it is read for.
    /// `CBaseAnimating::GetLastVisibleCycle` (`baseanimating.cpp:1043`) turns
    /// it into the cycle at which a non-looping sequence counts as *finished*
    /// — `1 - fadeouttime * cycleRate * playbackRate`, so a sequence played
    /// forwards is finished `fadeouttime` seconds before it ends — and
    /// `IsSequenceFinished()` is what `prop_testchamber_door` opens on.
    ///
    /// Worth carrying rather than assuming, because Valve's default is what
    /// content overwhelmingly writes and "overwhelmingly" is not "always":
    /// **10,664 of the 10,666 sequences in the shipped game are 0.2 and two
    /// are 0.5**. None is zero, so the term never folds away.
    pub fade_out_time: f32,
    /// Which [`Animation`] plays. Valve resolves this through a blend table of
    /// `groupsize[0] * groupsize[1]` entries; **every sequence in every model
    /// this port loads has a 1x1 table**, so only entry zero is read and the
    /// pose-parameter machinery that would pick another is not written.
    pub anim: usize,
}

/// One bone's channel within an animation, expanded to one value per frame.
///
/// A `Vec` of length 1 is a channel that does not change — Valve's
/// `STUDIO_ANIM_RAWPOS`/`RAWROT`/`RAWROT2`, and also a bone the animation does
/// not mention at all, which holds its bind pose.
#[derive(Debug, Clone)]
pub struct BoneTrack {
    pub bone: usize,
    pub pos: Vec<Vec3>,
    pub rot: Vec<Quat>,
}

impl BoneTrack {
    /// The position at a fractional frame, clamped at both ends.
    fn position(&self, frame: usize, s: f32) -> Vec3 {
        sample(&self.pos, frame, s, Vec3::ZERO, |a, b, t| a.lerp(b, t))
    }

    /// The rotation at a fractional frame.
    ///
    /// `QuaternionBlend` (`mathlib_base.cpp`) — align, then **normalized
    /// lerp**, not slerp. The difference is visible only on wide blends and
    /// this is Valve's.
    fn rotation(&self, frame: usize, s: f32) -> Quat {
        sample(&self.rot, frame, s, Quat::IDENTITY, quaternion_blend)
    }
}

fn sample<T: Copy>(
    values: &[T],
    frame: usize,
    s: f32,
    empty: T,
    blend: impl Fn(T, T, f32) -> T,
) -> T {
    match values.len() {
        0 => empty,
        1 => values[0],
        n => {
            let a = frame.min(n - 1);
            let b = (frame + 1).min(n - 1);
            match a == b || s <= 0.0 {
                true => values[a],
                false => blend(values[a], values[b], s),
            }
        }
    }
}

/// One animation. `mstudioanimdesc_t` (`studio.h:970`) with its RLE block
/// already expanded.
#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    pub fps: f32,
    pub flags: u32,
    /// `numframes`. At least 1.
    pub frame_count: usize,
    /// One entry per bone the animation actually mentions. A bone with no
    /// entry keeps its bind pose.
    pub tracks: Vec<BoneTrack>,
}

impl Animation {
    /// How long one play-through lasts, in seconds.
    ///
    /// `Studio_Duration`: `(numframes - 1) / fps`. A one-frame animation has
    /// **zero** duration, which is why [`pose`] must not divide by it.
    pub fn duration(&self) -> f32 {
        match self.fps > 0.0 {
            true => (self.frame_count.max(1) - 1) as f32 / self.fps,
            false => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

/// Reads the bone list. `numbones` / `boneindex` at header offsets 156 / 160.
pub(super) fn parse_bones(r: &Reader, base: usize, count: usize) -> Result<Vec<Bone>, StudioError> {
    let mut bones = Vec::with_capacity(count);
    for i in 0..count {
        let at = base + i * BONE_STRIDE;
        let name_at = r.relative_offset(at, at, "mstudiobone_t::sznameindex")?;
        let parent = r.i32(at + 4)?;
        // A parent index that is not *lower* than the child would make the
        // single forward pass in `pose` read a matrix that has not been
        // computed yet. `studiomdl` always writes them in order; this refuses
        // a file that does not rather than producing a silently wrong pose.
        let parent = match parent {
            p if p < 0 => None,
            p if (p as usize) < i => Some(p as usize),
            p => {
                return Err(r.corrupt(format!(
                    "bone {i} names parent {p}, which is not earlier in the list"
                )))
            }
        };
        bones.push(Bone {
            name: r.c_string(name_at)?,
            parent,
            pos: r.vec3(at + 32)?,
            quat: Quat::from_xyzw(
                r.f32(at + 44)?,
                r.f32(at + 48)?,
                r.f32(at + 52)?,
                r.f32(at + 56)?,
            ),
            rot: r.vec3(at + 60)?,
            pos_scale: r.vec3(at + 72)?,
            rot_scale: r.vec3(at + 84)?,
            pose_to_bone: matrix3x4(r, at + 96)?,
            flags: r.u32(at + 160)?,
        });
    }
    Ok(bones)
}

/// Reads the sequence table. `numlocalseq` / `localseqindex` at 188 / 192.
pub(super) fn parse_sequences(
    r: &Reader,
    base: usize,
    count: usize,
    anim_count: usize,
) -> Result<Vec<Sequence>, StudioError> {
    let mut sequences = Vec::with_capacity(count);
    for i in 0..count {
        let at = base + i * SEQUENCE_STRIDE;
        let label_at = r.relative_offset(at + 4, at, "mstudioseqdesc_t::szlabelindex")?;
        // `pBlend( 0, 0 )` — the first entry of the `groupsize[0] *
        // groupsize[1]` blend table, which is the only one without pose
        // parameters to index the rest.
        let blend_at = r.relative_offset(at + 60, at, "mstudioseqdesc_t::animindexindex")?;
        let anim = r.i16(blend_at)?;
        let anim = match anim >= 0 && (anim as usize) < anim_count {
            true => anim as usize,
            // `pAnimdesc` clamps an out-of-range index to 0 rather than
            // failing (`studio.h:1070`), and so does this.
            false => 0,
        };
        sequences.push(Sequence {
            label: r.c_string(label_at)?,
            flags: r.u32(at + 12)?,
            // `fadeouttime`, which sits after `fadeintime` at the end of the
            // pose-parameter block: `paramparent` at 100, `fadeintime` at 104.
            fade_out_time: r.f32(at + 108)?,
            anim,
        });
    }
    Ok(sequences)
}

/// Reads and expands the animations. `numlocalanim` / `localanimindex` at
/// 180 / 184.
pub(super) fn parse_animations(
    r: &Reader,
    base: usize,
    count: usize,
    bones: &[Bone],
) -> Result<Vec<Animation>, StudioError> {
    let mut animations = Vec::with_capacity(count);
    for i in 0..count {
        let at = base + i * ANIM_DESC_STRIDE;
        let name_at = r.relative_offset(at + 4, at, "mstudioanimdesc_t::sznameindex")?;
        let flags = r.u32(at + 12)?;
        let frame_count = r.i32(at + 16)?.max(1) as usize;
        // `animblock` non-zero means the data is in a companion `.ani` rather
        // than in this file. 68 files in the game have one and no model the
        // port loads is among them; the animation reads as empty, which holds
        // the bind pose, rather than reading the wrong bytes.
        let animblock = r.i32(at + 52)?;
        let anim_at = r.i32(at + 56)?;

        let readable = animblock == 0
            && anim_at > 0
            && flags & STUDIO_ALLZEROS == 0
            && flags & STUDIO_FRAMEANIM == 0;

        let tracks = match readable {
            true => parse_tracks(r, at + anim_at as usize, frame_count, bones)?,
            false => Vec::new(),
        };

        animations.push(Animation {
            name: r.c_string(name_at)?,
            fps: r.f32(at + 8)?,
            flags,
            frame_count,
            tracks,
        });
    }
    Ok(animations)
}

/// Walks the `mstudio_rle_anim_t` chain — one link per bone, each pointing at
/// the next by a byte offset from itself.
fn parse_tracks(
    r: &Reader,
    start: usize,
    frame_count: usize,
    bones: &[Bone],
) -> Result<Vec<BoneTrack>, StudioError> {
    let mut tracks = Vec::new();
    let mut at = start;
    // The bound is belt and braces against a corrupt file pointing a link at
    // itself; the chain's real ends are the two below.
    for _ in 0..=bones.len() {
        let bone = r.u8(at)?;
        // > **Bone 255 is the terminator, not a bone.** `studiomdl` writes a
        // > link with `bone = 255` after the last real one
        // > (`utils/studiomdl/write.cpp:1182`) and the decoder's loop is
        // > `while (panim && panim->bone < 255)` (`bone_decode.cpp:1395`).
        // > Treating it as a bone index refuses 15 of the models
        // > `sp_a1_intro1` alone places — which is how this was found, because
        // > every one of them still parsed as a *file*.
        if bone == 255 {
            break;
        }
        let bone = bone as usize;
        let flags = r.u8(at + 1)?;
        let next = r.i16(at + 2)?;
        let Some(base) = bones.get(bone) else {
            return Err(r.corrupt(format!("an animation names bone {bone}, which does not exist")));
        };

        // `pData()` — everything after the 4-byte header.
        let data = at + 4;
        // `pRotV()` is first and `pPosV()` follows it *only if* there is a
        // rotation channel (`studio.h:884`), which is the one place the two
        // offsets depend on each other.
        let rot_values = data;
        let pos_values = data + 6 * usize::from(flags & anim_flag::ANIMROT != 0);

        let rot = read_rotation(r, base, flags, rot_values, data, frame_count)?;
        let pos = read_position(r, base, flags, pos_values, data, frame_count)?;

        tracks.push(BoneTrack { bone, pos, rot });

        if next == 0 {
            break;
        }
        at = match at.checked_add_signed(next as isize) {
            Some(next) => next,
            None => return Err(r.corrupt("an animation link points before the file".to_owned())),
        };
    }
    Ok(tracks)
}

/// `CalcBoneQuaternion` (`bone_decode.cpp:150`), evaluated at every frame.
fn read_rotation(
    r: &Reader,
    bone: &Bone,
    flags: u8,
    values_at: usize,
    data: usize,
    frame_count: usize,
) -> Result<Vec<Quat>, StudioError> {
    if flags & anim_flag::RAWROT != 0 {
        return Ok(vec![quaternion48(r, data)?]);
    }
    if flags & anim_flag::RAWROT2 != 0 {
        return Ok(vec![quaternion64(r, data)?]);
    }
    if flags & anim_flag::ANIMROT == 0 {
        // No rotation channel: the bind pose, or identity for a delta.
        return Ok(vec![match flags & anim_flag::DELTA != 0 {
            true => Quat::IDENTITY,
            false => bone.quat,
        }]);
    }

    let delta = flags & anim_flag::DELTA != 0;
    let mut out = Vec::with_capacity(frame_count);
    for frame in 0..frame_count {
        // **The addition happens in Euler space and the conversion after it.**
        // Interpolating the quaternions of two frames is not the same as
        // interpolating their angles, and Valve does the latter here.
        let mut angle = Vec3::new(
            extract(r, values_at, 0, frame, bone.rot_scale.x)?,
            extract(r, values_at, 1, frame, bone.rot_scale.y)?,
            extract(r, values_at, 2, frame, bone.rot_scale.z)?,
        );
        if !delta {
            angle += bone.rot;
        }
        out.push(angle_quaternion(angle));
    }
    Ok(out)
}

/// `CalcBonePosition` (`bone_decode.cpp:273`), evaluated at every frame.
fn read_position(
    r: &Reader,
    bone: &Bone,
    flags: u8,
    values_at: usize,
    data: usize,
    frame_count: usize,
) -> Result<Vec<Vec3>, StudioError> {
    if flags & anim_flag::RAWPOS != 0 {
        // `pPos()` sits after whichever constant rotation is present.
        let at = data
            + 6 * usize::from(flags & anim_flag::RAWROT != 0)
            + 8 * usize::from(flags & anim_flag::RAWROT2 != 0);
        return Ok(vec![vector48(r, at)?]);
    }
    if flags & anim_flag::ANIMPOS == 0 {
        return Ok(vec![match flags & anim_flag::DELTA != 0 {
            true => Vec3::ZERO,
            false => bone.pos,
        }]);
    }

    let delta = flags & anim_flag::DELTA != 0;
    let mut out = Vec::with_capacity(frame_count);
    for frame in 0..frame_count {
        let mut pos = Vec3::new(
            extract(r, values_at, 0, frame, bone.pos_scale.x)?,
            extract(r, values_at, 1, frame, bone.pos_scale.y)?,
            extract(r, values_at, 2, frame, bone.pos_scale.z)?,
        );
        if !delta {
            pos += bone.pos;
        }
        out.push(pos);
    }
    Ok(out)
}

/// `ExtractAnimValue`'s single-value form (`bone_decode.cpp:114`) — the RLE
/// walk for one axis of one channel at one frame.
///
/// `mstudioanim_valueptr_t` is three `short` offsets, one per axis, each
/// relative to the struct; a zero offset is a channel that is not stored and
/// reads as zero. What it points at is a run of `mstudioanimvalue_t`, a union
/// of `{ valid, total }` bytes and a `short` value: `total` frames are covered
/// by this run and the first `valid` of them have their own value, the rest
/// repeating the last.
fn extract(
    r: &Reader,
    values_at: usize,
    axis: usize,
    frame: usize,
    scale: f32,
) -> Result<f32, StudioError> {
    let offset = r.i16(values_at + axis * 2)?;
    if offset <= 0 {
        return Ok(0.0);
    }
    let mut at = values_at + offset as usize;
    let mut k = frame;

    // Each step consumes one run; a stream cannot have more runs than frames.
    for _ in 0..=frame + 1 {
        let valid = r.u8(at)? as usize;
        let total = r.u8(at + 1)? as usize;
        if total == 0 {
            // "running off the end of the animation stream is bad" — Valve
            // asserts and returns zero, and a release build of the shipped
            // game does the latter.
            return Ok(0.0);
        }
        if total > k {
            let index = k.min(valid.saturating_sub(1));
            return Ok(f32::from(r.i16(at + 2 + index * 2)?) * scale);
        }
        k -= total;
        at += 2 * (valid + 1);
    }
    Ok(0.0)
}

// ---------------------------------------------------------------------------
// the compressed scalar types
// ---------------------------------------------------------------------------

/// `Quaternion48` (`compressed_vector.h`) — x:16, y:16, z:15, wneg:1.
fn quaternion48(r: &Reader, at: usize) -> Result<Quat, StudioError> {
    let x = f32::from(r.u16(at)?);
    let y = f32::from(r.u16(at + 2)?);
    let packed = r.u16(at + 4)?;
    let z = f32::from(packed & 0x7fff);
    let (x, y, z) = (
        (x - 32768.0) * (1.0 / 32768.5),
        (y - 32768.0) * (1.0 / 32768.5),
        (z - 16384.0) * (1.0 / 16384.5),
    );
    Ok(rebuild_w(x, y, z, packed & 0x8000 != 0))
}

/// `Quaternion64` — x:21, y:21, z:21, wneg:1, packed little-endian.
fn quaternion64(r: &Reader, at: usize) -> Result<Quat, StudioError> {
    let bits = r.u64(at)?;
    let field = |shift: u32| ((bits >> shift) & 0x1f_ffff) as f32;
    let (x, y, z) = (
        (field(0) - 1_048_576.0) * (1.0 / 1_048_576.5),
        (field(21) - 1_048_576.0) * (1.0 / 1_048_576.5),
        (field(42) - 1_048_576.0) * (1.0 / 1_048_576.5),
    );
    Ok(rebuild_w(x, y, z, bits >> 63 != 0))
}

/// The shared tail of both: `w` is whatever makes the quaternion unit length,
/// and only its **sign** is stored.
fn rebuild_w(x: f32, y: f32, z: f32, negative: bool) -> Quat {
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    Quat::from_xyzw(x, y, z, if negative { -w } else { w })
}

/// `Vector48` — three IEEE-754 half floats.
fn vector48(r: &Reader, at: usize) -> Result<Vec3, StudioError> {
    Ok(Vec3::new(
        f32::from(half::from_bits(r.u16(at)?)),
        f32::from(half::from_bits(r.u16(at + 2)?)),
        f32::from(half::from_bits(r.u16(at + 4)?)),
    ))
}

/// `float16` (`mathlib/float16.h`). Rust has `f16` only on nightly, so this is
/// the five lines of IEEE-754 binary16 decode rather than a dependency.
mod half {
    pub struct Half(f32);

    impl From<Half> for f32 {
        fn from(h: Half) -> f32 {
            h.0
        }
    }

    pub fn from_bits(bits: u16) -> Half {
        let sign = f32::from(bits >> 15);
        let exponent = i32::from((bits >> 10) & 0x1f);
        let mantissa = f32::from(bits & 0x3ff);
        let magnitude = match exponent {
            // Subnormal, including zero.
            0 => mantissa * 2f32.powi(-24),
            // Infinity and NaN. Valve's `GetFloat` builds the IEEE bit
            // pattern; the shipped models carry neither.
            31 => f32::INFINITY,
            e => (1.0 + mantissa / 1024.0) * 2f32.powi(e - 15),
        };
        Half(match sign == 0.0 {
            true => magnitude,
            false => -magnitude,
        })
    }
}

// ---------------------------------------------------------------------------
// the maths
// ---------------------------------------------------------------------------

/// `AngleQuaternion( const RadianEuler &, Quaternion & )`
/// (`mathlib_base.cpp:1076`).
///
/// > **A `RadianEuler` is not a `QAngle`.** Valve's own comment at the SIMD
/// > path reads *"the ordering here is different from the AngleQuaternion
/// > below because p, y, r are not in the same locations in QAngle +
/// > RadianEuler. Yay!"* — a `RadianEuler` is `(roll, pitch, yaw)` in radians
/// > where a `QAngle` is `(pitch, yaw, roll)` in degrees. Reading one as the
/// > other gives a plausible rotation about the wrong axis, which is why this
/// > is a function here rather than a call to [`crate::math::angle_matrix`].
fn angle_quaternion(angles: Vec3) -> Quat {
    let (sy, cy) = (angles.z * 0.5).sin_cos();
    let (sp, cp) = (angles.y * 0.5).sin_cos();
    let (sr, cr) = (angles.x * 0.5).sin_cos();

    let (sr_cp, cr_sp) = (sr * cp, cr * sp);
    let (cr_cp, sr_sp) = (cr * cp, sr * sp);
    Quat::from_xyzw(
        sr_cp * cy - cr_sp * sy,
        cr_sp * cy + sr_cp * sy,
        cr_cp * sy - sr_sp * cy,
        cr_cp * cy + sr_sp * sy,
    )
}

/// `QuaternionBlend` (`mathlib_base.cpp`) — `QuaternionAlign` then
/// `QuaternionBlendNoAlign`, which is a **normalized lerp and not a slerp**.
fn quaternion_blend(p: Quat, q: Quat, t: f32) -> Quat {
    // `QuaternionAlign`: flip the second one if it is the long way round.
    let q = match p.dot(q) < 0.0 {
        true => -q,
        false => q,
    };
    let blended = p * (1.0 - t) + q * t;
    match blended.length_squared() > 0.0 {
        true => blended.normalize(),
        false => p,
    }
}

/// A `matrix3x4_t`: row-major, three rows of four, translation in the last
/// column.
fn matrix3x4(r: &Reader, at: usize) -> Result<Mat4, StudioError> {
    let mut m = [0.0f32; 12];
    for (i, slot) in m.iter_mut().enumerate() {
        *slot = r.f32(at + i * 4)?;
    }
    // Valve indexes `m[row][col]`; `glam` is column-major, so the transpose is
    // in the indexing rather than in a call.
    Ok(Mat4::from_cols_array(&[
        m[0], m[4], m[8], 0.0, //
        m[1], m[5], m[9], 0.0, //
        m[2], m[6], m[10], 0.0, //
        m[3], m[7], m[11], 1.0,
    ]))
}

// ---------------------------------------------------------------------------
// posing
// ---------------------------------------------------------------------------

/// Where every bone is, in **model space**, at `cycle` through `anim`.
///
/// `R_StudioSetupBones` (`r_studio.cpp:250`) reduced to what a model with one
/// animation, no layers, no pose parameters and no IK needs:
///
/// 1. sample each bone's local position and rotation at the fractional frame,
/// 2. chain each bone onto its parent,
/// 3. multiply by [`Bone::pose_to_bone`], which is the step that makes the
///    result something a **bind-pose vertex** can be multiplied by.
///
/// `anim` of `None` — no sequence, or one whose animation carried no data —
/// gives the bind pose, which for every bone is exactly the identity once
/// step 3 has run.
///
/// `cycle` is clamped to `[0, 1]`; looping is the caller's, because whether a
/// sequence loops is a property of the *sequence* and this takes an animation.
pub fn pose(bones: &[Bone], anim: Option<&Animation>, cycle: f32) -> Vec<Mat4> {
    // `Studio_CalcFrame`: a cycle spans `numframes - 1` intervals, so the last
    // frame is reached at cycle 1 exactly.
    let (frame, s) = match anim {
        Some(anim) if anim.frame_count > 1 => {
            let position = cycle.clamp(0.0, 1.0) * (anim.frame_count - 1) as f32;
            (position.floor() as usize, position.fract())
        }
        _ => (0, 0.0),
    };

    let mut bone_to_model: Vec<Mat4> = Vec::with_capacity(bones.len());
    for (index, bone) in bones.iter().enumerate() {
        let track = anim.and_then(|a| a.tracks.iter().find(|t| t.bone == index));
        let (pos, rot) = match track {
            Some(track) => (track.position(frame, s), track.rotation(frame, s)),
            // A bone the animation does not mention holds its bind pose.
            None => (bone.pos, bone.quat),
        };

        let local = Mat4::from_rotation_translation(rot, pos);
        let world = match bone.parent {
            // The parent is always earlier in the list — `parse_bones`
            // refuses a file where it is not — so one forward pass is enough.
            Some(parent) => bone_to_model[parent] * local,
            None => local,
        };
        bone_to_model.push(world);
    }

    for (matrix, bone) in bone_to_model.iter_mut().zip(bones) {
        *matrix *= bone.pose_to_bone;
    }
    bone_to_model
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Euler convention, against `AngleQuaternion` evaluated by hand.
    /// Getting this wrong turns a button's plate about the wrong axis, which
    /// looks like a broken model rather than a broken decoder.
    #[test]
    fn a_radian_euler_is_roll_pitch_yaw_and_not_a_qangle() {
        let close = |a: Quat, b: Quat| {
            assert!((a.x - b.x).abs() < 1e-6, "{a:?} vs {b:?}");
            assert!((a.y - b.y).abs() < 1e-6, "{a:?} vs {b:?}");
            assert!((a.z - b.z).abs() < 1e-6, "{a:?} vs {b:?}");
            assert!((a.w - b.w).abs() < 1e-6, "{a:?} vs {b:?}");
        };

        // `angles.z` is yaw — a rotation about +Z.
        let yaw = angle_quaternion(Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2));
        close(yaw, Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        // `angles.y` is pitch — about +Y.
        let pitch = angle_quaternion(Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0));
        close(pitch, Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        // `angles.x` is roll — about +X. A `QAngle` would have put pitch here.
        let roll = angle_quaternion(Vec3::new(std::f32::consts::FRAC_PI_2, 0.0, 0.0));
        close(roll, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
    }

    /// `w` is reconstructed, not stored — only its sign is.
    #[test]
    fn a_compressed_quaternion_rebuilds_its_w() {
        let q = rebuild_w(0.5, 0.5, 0.5, false);
        assert!((q.length() - 1.0).abs() < 1e-6);
        assert!(q.w > 0.0);
        assert!((rebuild_w(0.5, 0.5, 0.5, true).w + q.w).abs() < 1e-6);
        // …and a vector that is already unit length leaves `w` at zero rather
        // than taking the square root of a small negative number.
        let q = rebuild_w(1.0, 0.0, 0.0, false);
        assert_eq!(q.w, 0.0);
    }

    /// The blend is a normalized lerp, so it stays unit length but does **not**
    /// travel at constant angular speed — which is what distinguishes it from
    /// a slerp and is Valve's.
    #[test]
    fn the_blend_aligns_and_normalizes() {
        let a = Quat::from_rotation_z(0.0);
        let b = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let half = quaternion_blend(a, b, 0.5);
        assert!((half.length() - 1.0).abs() < 1e-6);

        // Negating a quaternion names the same rotation; the align step is
        // what stops the blend taking the long way round through it.
        let flipped = quaternion_blend(a, -b, 0.5);
        assert!((half.dot(flipped).abs() - 1.0).abs() < 1e-6);
    }

    /// Half floats, against values whose binary16 form is exact.
    #[test]
    fn halves_decode() {
        assert_eq!(f32::from(half::from_bits(0x0000)), 0.0);
        assert_eq!(f32::from(half::from_bits(0x3c00)), 1.0);
        assert_eq!(f32::from(half::from_bits(0xbc00)), -1.0);
        assert_eq!(f32::from(half::from_bits(0x4000)), 2.0);
        assert_eq!(f32::from(half::from_bits(0x3800)), 0.5);
        // 13.64 — the button's plate offset, to the nearest half.
        assert!((f32::from(half::from_bits(0x4ad2)) - 13.640625).abs() < 1e-4);
    }

    /// A channel with no data reads as zero rather than as whatever follows it.
    #[test]
    fn a_zero_offset_channel_is_zero() {
        let bytes = [0u8; 16];
        let r = Reader {
            path: "test",
            what: "a .mdl",
            bytes: &bytes,
        };
        assert_eq!(extract(&r, 0, 0, 0, 1.0).expect("in range"), 0.0);
    }

    /// The RLE walk: one run of three frames, two of which carry a value and
    /// the third repeating the second.
    #[test]
    fn the_rle_walk_repeats_the_last_valid_value() {
        // offsets[3] = { 6, 0, 0 }, then at +6: valid=2, total=3, 10, 20.
        let mut bytes = vec![0u8; 32];
        bytes[0..2].copy_from_slice(&6i16.to_le_bytes());
        bytes[6] = 2; // valid
        bytes[7] = 3; // total
        bytes[8..10].copy_from_slice(&10i16.to_le_bytes());
        bytes[10..12].copy_from_slice(&20i16.to_le_bytes());
        let r = Reader {
            path: "test",
            what: "a .mdl",
            bytes: &bytes,
        };
        let at = |frame| extract(&r, 0, 0, frame, 2.0).expect("in range");
        assert_eq!(at(0), 20.0, "frame 0 -> 10 * scale");
        assert_eq!(at(1), 40.0, "frame 1 -> 20 * scale");
        // Frame 2 is inside `total` but past `valid`, so it repeats.
        assert_eq!(at(2), 40.0, "frame 2 repeats the last valid value");
    }

    /// A pose with no animation is the identity for every bone, whatever the
    /// bind pose is — which is the property that lets a static prop and an
    /// animated model share one draw path.
    #[test]
    fn the_bind_pose_is_the_identity() {
        let bones = vec![
            Bone {
                name: "root".to_owned(),
                parent: None,
                pos: Vec3::new(1.0, 2.0, 3.0),
                quat: Quat::from_rotation_z(0.3),
                rot: Vec3::ZERO,
                pos_scale: Vec3::ONE,
                rot_scale: Vec3::ONE,
                flags: 0,
                pose_to_bone: Mat4::from_rotation_translation(
                    Quat::from_rotation_z(0.3),
                    Vec3::new(1.0, 2.0, 3.0),
                )
                .inverse(),
            },
            Bone {
                name: "child".to_owned(),
                parent: Some(0),
                pos: Vec3::new(0.0, 13.64, 0.0),
                quat: Quat::IDENTITY,
                rot: Vec3::ZERO,
                pos_scale: Vec3::ONE,
                rot_scale: Vec3::ONE,
                flags: 0,
                pose_to_bone: (Mat4::from_rotation_translation(
                    Quat::from_rotation_z(0.3),
                    Vec3::new(1.0, 2.0, 3.0),
                ) * Mat4::from_translation(Vec3::new(0.0, 13.64, 0.0)))
                .inverse(),
            },
        ];

        for matrix in pose(&bones, None, 0.0) {
            let difference: f32 = (matrix - Mat4::IDENTITY)
                .to_cols_array()
                .iter()
                .map(|x| x.abs())
                .sum();
            assert!(difference < 1e-4, "{matrix:?} is not the identity");
        }
    }
}
