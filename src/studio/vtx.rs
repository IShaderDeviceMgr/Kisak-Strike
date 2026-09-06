//! Reading `.dx90.vtx` — the studio index buffer.
//!
//! `OptimizedModel::FileHeader_t` and the hierarchy under it
//! (`public/optimize.h`, 264 lines and entirely `#pragma pack(1)`). The nesting
//! mirrors the `.mdl`'s one level deeper:
//!
//! ```text
//! FileHeader_t
//!   BodyPartHeader_t[]        ← must match the .mdl's body parts
//!     ModelHeader_t[]         ← must match that body part's models
//!       ModelLODHeader_t[]
//!         MeshHeader_t[]      ← must match that model's meshes
//!           StripGroupHeader_t[]
//!             Vertex_t[], u16 indices, StripHeader_t[]
//! ```
//!
//! As in the `.mdl`, every offset is relative to the struct holding it.
//!
//! # What a static prop never reaches
//!
//! Measured over Portal 2's 968 static prop models: every strip group's flags
//! are exactly `STRIPGROUP_IS_HWSKINNED`, so none is `IS_DELTA_FLEXED`; every
//! strip is `STRIP_IS_TRILIST`, so none is a sub-d quad list; and
//! `StripHeader_t::numBones` is 0 on all 1,130 strip groups, so the
//! bone-state-change path is never entered. This reader therefore keeps the
//! *indices* and drops the strips' bone plumbing, and treats a quad list as an
//! error rather than silently drawing it as triangles.
//!
//! 63 strip groups do carry topology indices — sub-d control data — but their
//! strips are still trilists, so the topology block is skipped rather than
//! read.

use super::mdl::Reader;
use super::StudioError;

/// `OPTIMIZED_MODEL_FILE_VERSION`, `optimize.h:19`. All 4,066 `.vtx` files
/// Portal 2 ships are this version.
const VERSION: i32 = 7;

/// `sizeof(FileHeader_t)`.
const HEADER_SIZE: usize = 36;

/// `sizeof(BodyPartHeader_t)`.
const BODY_PART_STRIDE: usize = 8;

/// `sizeof(ModelHeader_t)`.
const MODEL_STRIDE: usize = 8;

/// `sizeof(ModelLODHeader_t)`.
const LOD_STRIDE: usize = 12;

/// `sizeof(MeshHeader_t)` — 8 bytes of fields plus a `flags` byte, packed.
const MESH_STRIDE: usize = 9;

/// `sizeof(StripGroupHeader_t)` — 24 bytes of fields, a `flags` byte, then
/// `numTopologyIndices`/`topologyOffset`.
const STRIP_GROUP_STRIDE: usize = 33;

/// `sizeof(StripHeader_t)`.
const STRIP_STRIDE: usize = 35;

/// `sizeof(Vertex_t)` — `boneWeightIndex[3]`, `numBones`, `origMeshVertID`,
/// `boneID[3]`.
const VERTEX_STRIDE: usize = 9;

/// `offsetof(Vertex_t, origMeshVertID)`.
///
/// Named rather than inlined because getting it wrong is the format's quietest
/// failure: the field is a `u16` straddling `numBones` and `boneID[0]`, so a
/// reader that is one byte out still returns a small plausible number for every
/// vertex of every model and simply draws the wrong triangles.
pub(super) const ORIG_MESH_VERT_ID: usize = 4;

/// `STRIPGROUP_IS_HWSKINNED`.
const STRIPGROUP_IS_HWSKINNED: u8 = 0x02;

/// `STRIPGROUP_IS_DELTA_FLEXED`.
const STRIPGROUP_IS_DELTA_FLEXED: u8 = 0x04;

/// `STRIP_IS_TRILIST`.
const STRIP_IS_TRILIST: u8 = 0x01;

/// `STRIP_IS_QUADLIST_REG` | `STRIP_IS_QUADLIST_EXTRA`.
const STRIP_IS_QUADLIST: u8 = 0x02 | 0x04;

/// One mesh's triangles, as indices **relative to that mesh**.
///
/// One level of the format's indirection is flattened here — a strip group's
/// index array names entries in the group's own `Vertex_t` table, and this
/// resolves each to its `origMeshVertID`. The remaining two additions (the
/// mesh's `vertexoffset` and the model's base in the pool) belong to
/// [`build`](super::build), because only it holds the `.mdl` as well.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mesh {
    /// Triangle list, three indices per triangle, in the file's winding.
    pub indices: Vec<u32>,
    /// The mesh's **hardware vertex order**: every strip group's `Vertex_t`
    /// table, concatenated, as mesh-relative vertex ids.
    ///
    /// This is the order the original's GPU vertex buffer would be in, because
    /// `studiomeshgroup_t` is built by walking exactly this table — and it is
    /// therefore the order a `.vhv` writes its colours in. It is **not** the
    /// `.vvd` pool order and is not a prefix of it: a model whose lower LODs
    /// use a subset of the pool has more pool vertices than LOD 0 has hardware
    /// vertices, and the ids are a permutation rather than a run.
    ///
    /// Kept because [`vhv`](crate::studio::vhv) is the only way to read a
    /// prop's baked lighting and this is the only record of that order.
    pub hardware: Vec<u32>,
}

/// One LOD of one model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lod {
    pub meshes: Vec<Mesh>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Model {
    pub lods: Vec<Lod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BodyPart {
    pub models: Vec<Model>,
}

/// A parsed `.vtx`.
#[derive(Debug, Clone)]
pub struct Vtx {
    pub path: String,
    /// Must equal the `.mdl`'s and `.vvd`'s.
    pub checksum: u32,
    pub lod_count: usize,
    pub body_parts: Vec<BodyPart>,
}

impl Vtx {
    /// Parses a `.vtx`, keeping every LOD.
    ///
    /// Every LOD is kept rather than just the root because they are cheap —
    /// indices only — and because LOD selection later is then a matter of
    /// choosing, not of re-reading.
    pub fn parse(path: String, bytes: &[u8]) -> Result<Vtx, StudioError> {
        let r = Reader {
            path: &path,
            what: "a .vtx",
            bytes,
        };

        if bytes.len() < HEADER_SIZE {
            return Err(StudioError::TooShort {
                path,
                what: "an optimized model header",
                size: bytes.len(),
                needed: HEADER_SIZE,
            });
        }

        // A `.vtx` has no magic number — `version` is the first field, which is
        // the only guard the format offers against being handed the wrong file.
        let version = r.i32(0)?;
        if version != VERSION {
            return Err(StudioError::Version {
                path,
                what: ".vtx",
                version,
                expected: VERSION,
            });
        }

        // `FileHeader_t`'s field offsets, which are not evenly spaced:
        // `maxBonesPerStrip`/`maxBonesPerFace` are `unsigned short` at 8 and 10
        // and `maxBonesPerVert` is an `int` at 12, so `checkSum` lands at 16.
        let checksum = r.u32(16)?;
        let lod_count = r.count(20, "LODs")?;
        let body_part_count = r.count(28, "body parts")?;
        let base = r.offset(32, "bodyPartOffset")?;

        let mut body_parts = Vec::with_capacity(body_part_count);
        for i in 0..body_part_count {
            body_parts.push(Self::body_part(&r, base + i * BODY_PART_STRIDE)?);
        }

        Ok(Vtx {
            path,
            checksum,
            lod_count,
            body_parts,
        })
    }

    fn body_part(r: &Reader, at: usize) -> Result<BodyPart, StudioError> {
        let count = r.count(at, "models")?;
        let base = r.relative_offset(at + 4, at, "BodyPartHeader_t::modelOffset")?;
        let mut models = Vec::with_capacity(count);
        for i in 0..count {
            models.push(Self::model(r, base + i * MODEL_STRIDE)?);
        }
        Ok(BodyPart { models })
    }

    fn model(r: &Reader, at: usize) -> Result<Model, StudioError> {
        let count = r.count(at, "LODs")?;
        let base = r.relative_offset(at + 4, at, "ModelHeader_t::lodOffset")?;
        let mut lods = Vec::with_capacity(count);
        for i in 0..count {
            lods.push(Self::lod(r, base + i * LOD_STRIDE)?);
        }
        Ok(Model { lods })
    }

    fn lod(r: &Reader, at: usize) -> Result<Lod, StudioError> {
        let count = r.count(at, "meshes")?;
        let base = r.relative_offset(at + 4, at, "ModelLODHeader_t::meshOffset")?;
        let mut meshes = Vec::with_capacity(count);
        for i in 0..count {
            meshes.push(Self::mesh(r, base + i * MESH_STRIDE)?);
        }
        Ok(Lod { meshes })
    }

    fn mesh(r: &Reader, at: usize) -> Result<Mesh, StudioError> {
        let count = r.count(at, "strip groups")?;
        let base = r.relative_offset(at + 4, at, "MeshHeader_t::stripGroupHeaderOffset")?;

        let mut indices = Vec::new();
        let mut hardware = Vec::new();
        for i in 0..count {
            Self::strip_group(
                r,
                base + i * STRIP_GROUP_STRIDE,
                i,
                &mut indices,
                &mut hardware,
            )?;
        }
        Ok(Mesh { indices, hardware })
    }

    /// Appends one strip group's triangles, resolved through its vertex table.
    ///
    /// A strip group owns a vertex table and an index array; the indices name
    /// entries in *that table*, and each entry's `origMeshVertID` names the
    /// real vertex. Both levels are collapsed here, so a `Mesh` holds indices
    /// straight into the mesh's vertex range.
    fn strip_group(
        r: &Reader,
        at: usize,
        which: usize,
        out: &mut Vec<u32>,
        hardware: &mut Vec<u32>,
    ) -> Result<(), StudioError> {
        let vertex_count = r.count(at, "strip group vertices")?;
        let vertex_base = r.relative_offset(at + 4, at, "StripGroupHeader_t::vertOffset")?;
        let index_count = r.count(at + 8, "strip group indices")?;
        let index_base = r.relative_offset(at + 12, at, "StripGroupHeader_t::indexOffset")?;
        let strip_count = r.count(at + 16, "strips")?;
        let strip_base = r.relative_offset(at + 20, at, "StripGroupHeader_t::stripOffset")?;
        let flags = *r
            .bytes
            .get(at + 24)
            .ok_or_else(|| r.corrupt(format!("strip group {which} has no flags byte")))?;

        if flags & STRIPGROUP_IS_DELTA_FLEXED != 0 {
            return Err(r.corrupt(format!(
                "strip group {which} is flex-delta data, which this engine does not read \
                 (no static prop in Portal 2 has any)"
            )));
        }
        // Recorded, not required: every shipped static prop strip group sets it,
        // but hardware skinning is about *how* the bones reach the shader and a
        // one-bone model does not care either way.
        let _ = STRIPGROUP_IS_HWSKINNED;

        // Resolve the group's vertex table once — `origMeshVertID` at +4 of a
        // 9-byte `Vertex_t`, after `boneWeightIndex[3]` and `numBones`, and
        // before `boneID[3]`.
        let mut mesh_vert = Vec::with_capacity(vertex_count);
        for i in 0..vertex_count {
            mesh_vert.push(r.u16(vertex_base + i * VERTEX_STRIDE + ORIG_MESH_VERT_ID)? as u32);
        }
        // The table itself, kept in order — see [`Mesh::hardware`]. Groups
        // concatenate, which is what makes a mesh's hardware vertex `j` the
        // `j`th entry across all of its groups.
        hardware.extend_from_slice(&mesh_vert);

        // Strips, not the group's whole index array, are what actually gets
        // drawn: a group's array can hold ranges no strip references. In every
        // shipped file there is exactly one strip covering the lot, but
        // walking the strips is what the original does and costs nothing.
        for i in 0..strip_count {
            let strip_at = strip_base + i * STRIP_STRIDE;
            let strip_index_count = r.count(strip_at, "strip indices")?;
            let strip_index_offset = r.count(strip_at + 4, "strip index offset")?;
            let strip_flags = *r
                .bytes
                .get(strip_at + 18)
                .ok_or_else(|| r.corrupt(format!("strip {i} has no flags byte")))?;

            if strip_flags & STRIP_IS_QUADLIST != 0 {
                return Err(r.corrupt(format!(
                    "strip {i} of strip group {which} is a sub-d quad list, which this \
                     engine does not read (no static prop in Portal 2 has any)"
                )));
            }
            // One shipped strip has flags 0 rather than STRIP_IS_TRILIST. It is
            // a trilist — the flag is simply unset — so an unflagged strip is
            // read as one rather than refused.
            let _ = STRIP_IS_TRILIST;

            if strip_index_offset + strip_index_count > index_count {
                return Err(r.corrupt(format!(
                    "strip {i} of strip group {which} covers indices {strip_index_offset}..{} \
                     of {index_count}",
                    strip_index_offset + strip_index_count
                )));
            }
            if strip_index_count % 3 != 0 {
                return Err(r.corrupt(format!(
                    "strip {i} of strip group {which} has {strip_index_count} indices, \
                     which is not a whole number of triangles"
                )));
            }

            out.reserve(strip_index_count);
            for j in strip_index_offset..strip_index_offset + strip_index_count {
                let slot = r.u16(index_base + j * 2)? as usize;
                let vertex = mesh_vert.get(slot).ok_or_else(|| {
                    r.corrupt(format!(
                        "index {j} of strip group {which} names vertex {slot} of {vertex_count}"
                    ))
                })?;
                out.push(*vertex);
            }
        }

        Ok(())
    }
}
