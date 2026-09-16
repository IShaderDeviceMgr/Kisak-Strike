//! Reading `.vvd` — the studio vertex pool.
//!
//! `vertexFileHeader_t` (`public/studio.h:2309`) and the fixup algorithm from
//! `Studio_LoadVertexes` (`public/studio.h:3776`) — an `inline` function in a
//! header, which is why it appears in no `.cpp` inventory and is easy to miss
//! entirely.
//!
//! # The fixup table is the whole difficulty of this format
//!
//! The vertex pool is stored **sorted by LOD**, so that culling to a coarser
//! root LOD is a prefix of the array. The fixup table puts it back into **mesh
//! order**, which is the order `.vtx` indices assume. Skipping it does not
//! error — it draws the model with its vertices shuffled.
//!
//! It is rare enough to be easy to forget and not rare enough to skip:
//! **measured over the 968 models Portal 2 places as static props, only 15 have
//! a fixup table at all, and 14 of those 15 are a genuine permutation even at
//! root LOD 0.** So getting it wrong leaves 954 models perfect and 14 visibly
//! scrambled, which is about the most confusing failure the format offers.
//! `studiomdl` writes a table only when a model has several meshes *and*
//! several LODs (`utils/studiomdl/write.cpp:4382`), which is why it is rare.
//!
//! The tangent array is permuted by the same table in lockstep. Permuting one
//! and not the other yields correct silhouettes with wrong lighting.

use super::mdl::Reader;
use super::StudioError;
use glam::{Vec2, Vec3, Vec4};

/// `MODEL_VERTEX_FILE_ID`, `"IDSV"`. `studio.h:2302`.
const IDENT: u32 = u32::from_le_bytes(*b"IDSV");

/// `MODEL_VERTEX_FILE_THIN_ID`, `"IDCV"` — a pool `CMDLCache::CreateThinVertexes`
/// rewrote to drop everything but position and bone weights, under console
/// memory pressure. Recognized only so the error can say what the file is; no
/// shipped file is one.
const THIN_IDENT: u32 = u32::from_le_bytes(*b"IDCV");

/// `MODEL_VERTEX_FILE_NULL_ID`, `"IDDV"` — a pool whose vertex data was
/// discarded outright (`CreateNullVertexes`). Same treatment.
const NULL_IDENT: u32 = u32::from_le_bytes(*b"IDDV");

/// `MODEL_VERTEX_FILE_VERSION`, `studio.h:2303`.
const VERSION: i32 = 4;

/// `MAX_NUM_LODS`.
const MAX_LODS: usize = 8;

/// `sizeof(vertexFileHeader_t)` through `tangentDataStart`.
const HEADER_SIZE: usize = 64;

/// `sizeof(mstudiovertex_t)` — "NOTE: This is exactly 48 bytes" (`studio.h:1446`).
const VERTEX_STRIDE: usize = 48;

/// `sizeof(Vector4D)`.
const TANGENT_STRIDE: usize = 16;

/// `sizeof(vertexFileFixup_t)`, `studio.h:2380`.
const FIXUP_STRIDE: usize = 12;

/// `mstudioboneweight_t` (`studio.h:284`) — which bones move this vertex.
///
/// Three slots, of which `count` are used. **Every vertex in every model this
/// port draws uses exactly one**, which is the measurement the whole animation
/// path is built on: `bone[0]` alone decides where the vertex goes, so a model
/// can be drawn by splitting its triangles between bones instead of skinning
/// them. [`StudioModel::rigid_bone`] is where that is checked rather than
/// assumed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoneWeights {
    /// `bone[3]`. Indices into the `.mdl`'s bone list.
    pub bones: [u8; 3],
    /// `weight[3]`, summing to 1 over the first `count`.
    pub weights: [f32; 3],
    /// `numbones`.
    pub count: u8,
}

impl BoneWeights {
    /// The one bone that moves this vertex, if exactly one does.
    pub fn rigid(&self) -> Option<u8> {
        match self.count {
            1 => Some(self.bones[0]),
            _ => None,
        }
    }
}

impl Default for BoneWeights {
    /// What a model with no bone data means: bone 0, weight 1.
    fn default() -> BoneWeights {
        BoneWeights {
            bones: [0; 3],
            weights: [1.0, 0.0, 0.0],
            count: 1,
        }
    }
}

/// One vertex.
///
/// `mstudiovertex_t` leads with a 16-byte `mstudioboneweight_t`, which for
/// every static prop in the game is "one bone, weight 1" — which is why this
/// was read past and discarded until animation landed. It is kept now, because
/// a `prop_floor_button`'s 7,929 vertices are split 7,263 on the body and 666
/// on the plate that moves, and that split *is* the animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub texcoord: Vec2,
    pub bones: BoneWeights,
}

/// A parsed `.vvd`, already reordered into mesh order for one root LOD.
#[derive(Debug, Clone)]
pub struct Vvd {
    pub path: String,
    pub checksum: u32,
    /// Which LOD [`vertices`](Vvd::vertices) was built for. 0 — the finest —
    /// until LOD selection lands.
    pub root_lod: usize,
    pub vertices: Vec<Vertex>,
    /// Tangent S in `xyz`, the binormal's sign in `w`. Parallel to
    /// [`vertices`](Vvd::vertices) and permuted with it.
    ///
    /// Empty when the file carries no tangent block, which
    /// [`Vvd::tangent`] answers with a `+x` tangent rather than making every
    /// caller branch.
    pub tangents: Vec<Vec4>,
}

impl Vvd {
    /// Parses a `.vvd` at root LOD 0.
    pub fn parse(path: String, bytes: &[u8]) -> Result<Vvd, StudioError> {
        Self::parse_lod(path, bytes, 0)
    }

    /// Parses a `.vvd`, culling to `root_lod`.
    ///
    /// `root_lod` past the file's LOD count is clamped, matching
    /// `CMDLCache::LoadVertexData`, which clamps rather than failing because
    /// the root LOD is a *quality setting* and a model with fewer LODs than the
    /// setting asks for is not an error.
    pub fn parse_lod(path: String, bytes: &[u8], root_lod: usize) -> Result<Vvd, StudioError> {
        let r = Reader {
            path: &path,
            what: "a .vvd",
            bytes,
        };

        if bytes.len() < HEADER_SIZE {
            return Err(StudioError::TooShort {
                path,
                what: "a vertex file header",
                size: bytes.len(),
                needed: HEADER_SIZE,
            });
        }

        let ident = r.u32(0)?;
        if ident != IDENT {
            return Err(StudioError::BadIdent {
                path,
                what: match ident {
                    THIN_IDENT => ".vvd (this one has thinned vertex data)",
                    NULL_IDENT => ".vvd (this one has had its vertex data discarded)",
                    _ => ".vvd",
                },
                ident,
                expected: IDENT,
            });
        }
        let version = r.i32(4)?;
        if version != VERSION {
            return Err(StudioError::Version {
                path,
                what: ".vvd",
                version,
                expected: VERSION,
            });
        }

        let checksum = r.u32(8)?;
        let lod_count = r.count(12, "LODs")?;
        let mut lod_vertex_counts = [0usize; MAX_LODS];
        for (i, count) in lod_vertex_counts.iter_mut().enumerate() {
            *count = r.count(16 + i * 4, "LOD vertexes")?;
        }
        let fixup_count = r.count(48, "fixups")?;
        let fixup_start = r.offset(52, "fixupTableStart")?;
        let vertex_start = r.offset(56, "vertexDataStart")?;
        let tangent_start = r.offset(60, "tangentDataStart")?;

        if lod_count == 0 {
            return Err(r.corrupt("the file declares no LODs".to_owned()));
        }
        let root_lod = root_lod.min(lod_count - 1);
        let wanted = lod_vertex_counts[root_lod];

        // The source pool is always sized by LOD 0 — the finest LOD is the
        // superset every coarser one is culled from.
        let source_count = lod_vertex_counts[0];
        let has_tangents = tangent_start != 0;

        let read_vertex = |index: usize| -> Result<Vertex, StudioError> {
            let at = vertex_start + index * VERTEX_STRIDE;
            // 0..16 is `mstudioboneweight_t`: three floats, three signed
            // bytes, and a count.
            let count = r.u8(at + 15)?.min(3);
            let mut bones = [0u8; 3];
            let mut weights = [0.0f32; 3];
            for slot in 0..count as usize {
                weights[slot] = r.f32(at + slot * 4)?;
                bones[slot] = r.u8(at + 12 + slot)?;
            }
            Ok(Vertex {
                position: r.vec3(at + 16)?,
                normal: r.vec3(at + 28)?,
                texcoord: Vec2::new(r.f32(at + 40)?, r.f32(at + 44)?),
                bones: BoneWeights {
                    bones,
                    weights,
                    count,
                },
            })
        };
        let read_tangent = |index: usize| -> Result<Vec4, StudioError> {
            let at = tangent_start + index * TANGENT_STRIDE;
            Ok(Vec4::new(
                r.f32(at)?,
                r.f32(at + 4)?,
                r.f32(at + 8)?,
                r.f32(at + 12)?,
            ))
        };

        let mut vertices = Vec::with_capacity(wanted);
        let mut tangents = Vec::with_capacity(if has_tangents { wanted } else { 0 });

        if fixup_count == 0 {
            // No table: the first `wanted` vertices are already in mesh order.
            for index in 0..wanted {
                vertices.push(read_vertex(index)?);
                if has_tangents {
                    tangents.push(read_tangent(index)?);
                }
            }
        } else {
            // `Studio_LoadVertexes` (`studio.h:3873`): walk the table in order,
            // skipping runs belonging to a finer LOD than the one wanted, and
            // concatenate what remains. `target` is implicit in `push` here;
            // the C++ carries it explicitly because it is writing into a
            // pre-sized buffer.
            for i in 0..fixup_count {
                let at = fixup_start + i * FIXUP_STRIDE;
                let lod = r.i32(at)?;
                let source = r.i32(at + 4)?;
                let count = r.i32(at + 8)?;

                if lod < 0 || source < 0 || count < 0 {
                    return Err(r.corrupt(format!(
                        "fixup {i} is {{ lod {lod}, source {source}, count {count} }}"
                    )));
                }
                // "working bottom up, skip over copying higher detail lods"
                if (lod as usize) < root_lod {
                    continue;
                }
                let (source, count) = (source as usize, count as usize);
                if source + count > source_count {
                    return Err(r.corrupt(format!(
                        "fixup {i} copies vertices {source}..{} of {source_count}",
                        source + count
                    )));
                }
                for index in source..source + count {
                    vertices.push(read_vertex(index)?);
                    if has_tangents {
                        tangents.push(read_tangent(index)?);
                    }
                }
            }

            // The table is the file's own claim about how many vertices the
            // root LOD has; `numLODVertexes` is the other. They must agree, or
            // the `.vtx`'s indices are measured against a length this does not
            // have.
            if vertices.len() != wanted {
                return Err(r.corrupt(format!(
                    "the fixup table yields {} vertices for LOD {root_lod} but the header \
                     declares {wanted}",
                    vertices.len()
                )));
            }
        }

        Ok(Vvd {
            path,
            checksum,
            root_lod,
            vertices,
            tangents,
        })
    }

    /// The tangent for vertex `index`, or `+x` with a positive binormal when
    /// the file has no tangent block.
    ///
    /// A zero `w` would mirror every bumped surface's lighting along V
    /// (`ModelVertex::tangent`), so the fallback is a real value rather than a
    /// zeroed one.
    pub fn tangent(&self, index: usize) -> Vec4 {
        self.tangents
            .get(index)
            .copied()
            .unwrap_or(Vec4::new(1.0, 0.0, 0.0, 1.0))
    }
}
