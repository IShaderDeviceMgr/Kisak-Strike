//! Studio models — reading `.mdl` / `.vvd` / `.vtx` into drawable geometry.
//!
//! Replaces the parts of `datacache/mdlcache.cpp` that read a model off disk
//! and the parts of `studiorender/r_studiodraw.cpp` that turn it into vertex
//! and index buffers. Design and the measurements that scoped it:
//! `portdocs/STUDIO.md`.
//!
//! A studio model is three files that must be read together:
//!
//! | | Holds | Reference |
//! |---|---|---|
//! | `.mdl` | the hierarchy, materials, bounds, flags | `public/studio.h:2532` |
//! | `.vvd` | the vertex pool — position, normal, texcoord, tangent | `public/studio.h:2309` |
//! | `.dx90.vtx` | the index buffer, per mesh | `public/optimize.h` |
//!
//! None is useful alone: the `.mdl` says which meshes exist and what material
//! each wears, the `.vtx` says which vertices each mesh's triangles use, and
//! the `.vvd` holds the vertices those indices name. All three carry the same
//! `checksum` and [`load`] refuses a trio that disagrees.
//!
//! # Why this is a top-level module
//!
//! It reads asset files and produces GPU-ready geometry, needing no map, no
//! engine and no window — the same shape as [`materials`](crate::materials)'
//! `Vtf` and `Vmt`, and the reason those live where they do. It names
//! `filesystem` and `materials` and nothing else, which is what lets every test
//! below run without an `Engine`.
//!
//! Where the *instances* live is a different question with a different answer:
//! a static prop's placement comes out of the `.bsp` and dies with the map, so
//! it belongs to `engine::world`, exactly as the lightmap pages do.
//!
//! # What this is not
//!
//! `CMDLCache` is a *cache manager* far more than it is a reader: LRU eviction,
//! a fixed memory budget, async load queues, lock/unlock refcounting and
//! `CreateThinVertexes`/`CreateNullVertexes` fallbacks that throw away vertex
//! data under pressure. All of that existed to fit models into a 2007 console's
//! memory, which is not a problem this port has. A `StudioModel` is an owned
//! value; dropping it frees it.
//!
//! Also absent, and **absent from the data rather than deferred**: skinning,
//! flexes/morphs and sub-division surfaces. Measured over the 968 models Portal
//! 2 places as static props, every one has exactly one bone, every strip group
//! is `STRIPGROUP_IS_HWSKINNED` with no `STRIPGROUP_IS_DELTA_FLEXED`, every
//! strip is `STRIP_IS_TRILIST`, and `StripHeader_t::numBones` is 0 throughout.
//! `portdocs/STUDIO.md` §3 has the full table. Animated models will need all
//! three back; static props reach none of them.
//!
//! # LOD
//!
//! Stage 1 reads **LOD 0 only**. 819 of those 968 models have exactly one LOD,
//! so this is the whole model for most of them and the highest-detail one for
//! the rest. [`Vvd`] already applies the root-LOD fixup machinery, so adding
//! LOD selection later is a parameter, not a rewrite.

// Stage 1 builds the readers; the first caller arrives with the `sprp` lump in
// stage 2 and the draw in stage 3 (`portdocs/STUDIO.md` §8). Until then every
// public item here is dead to `cargo build` and exercised only by the tests.
// Remove this when `engine::world` places props.
#![allow(dead_code)]

pub mod anim;
mod build;
#[cfg(test)]
mod fixture;
mod include;
mod mdl;
pub mod vhv;
mod vtx;
mod vvd;

pub use mdl::{Mdl, StudioFlags};
pub use vhv::Vhv;
pub use vtx::Vtx;
pub use vvd::Vvd;

use crate::filesystem::Vfs;
use crate::materials::mesh::ModelVertex;
use glam::Vec3;

/// The `.vtx` variant to open.
///
/// `CMDLCache::GetVTXExtension` (`datacache/mdlcache.cpp:3492`) returns this
/// unconditionally — the `.dx80.vtx` / `.sw.vtx` variants other Source branches
/// choose between survive in this tree only inside
/// `engine/MapReslistGenerator.cpp`'s file-list generator, never in the runtime
/// path.
///
/// Portal 2 also ships a suffix-less `.vtx` beside every `.dx90.vtx`; over 300
/// sampled pairs the two are byte-identical, so which one is opened is a
/// question of matching the reference rather than of content.
const VTX_EXTENSION: &str = ".dx90.vtx";

/// Anything that can go wrong reading a studio model.
///
/// Unlike a material or a texture, a model has **no error fallback** — there is
/// no equivalent of the magenta checkerboard for geometry, and a caller that
/// fails to load one should draw nothing rather than something wrong. So this
/// is a `Result` all the way out, and the decision to carry on without the
/// model belongs to whoever asked for it.
#[derive(Debug, thiserror::Error)]
pub enum StudioError {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: crate::filesystem::VfsError,
    },

    #[error("{path} is {size} bytes, too short to hold {what} ({needed} bytes)")]
    TooShort {
        path: String,
        what: &'static str,
        size: usize,
        needed: usize,
    },

    #[error("{path} is not a {what} file: identifier {ident:#010x}, expected {expected:#010x}")]
    BadIdent {
        path: String,
        what: &'static str,
        ident: u32,
        expected: u32,
    },

    /// Every shipped Portal 2 model is `.mdl` 49 / `.vvd` 4 / `.vtx` 7, so
    /// there is no version branching to write and anything else is refused
    /// rather than guessed at. `portdocs/STUDIO.md` §4.
    #[error("{path} is {what} version {version}; this engine reads version {expected}")]
    Version {
        path: String,
        what: &'static str,
        version: i32,
        expected: i32,
    },

    /// The format's own guard against a stale file. A `.vtx` built from a
    /// different revision of a `.mdl` does not fail to parse — it indexes the
    /// wrong vertices — so this is checked rather than trusted.
    #[error("{path} has checksum {found:#010x} but {mdl_path} has {expected:#010x}; the model's files are from different builds")]
    ChecksumMismatch {
        path: String,
        mdl_path: String,
        found: u32,
        expected: u32,
    },

    #[error("{path} is internally inconsistent: {what}")]
    Corrupt { path: String, what: String },
}

/// One `(material, index range)` draw.
///
/// This is `world`'s per-(material, page) batch with the lightmap page dropped
/// — deliberately the same word, because it is the same idea and was the same
/// idea in the original: Valve's *sort ID*, computed at load so the renderer
/// never sorts per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// The material name as [`MaterialCache::load`] wants it: relative to
    /// `materials/`, lowercased, no extension.
    ///
    /// [`MaterialCache::load`]: crate::materials::MaterialCache::load
    pub material: String,
    /// Offset into [`StudioModel::indices`].
    pub first_index: u32,
    pub index_count: u32,
    /// This batch's indices, split into contiguous runs by the **bone** that
    /// moves them.
    ///
    /// One run covering the whole batch for a model with one bone, which is
    /// every static prop in the game. See
    /// [`StudioModel::rigid_bones`] for why the split is a substitute for
    /// skinning and when it stops being one.
    pub bones: Vec<BoneRun>,
    /// Which body part and model within it this came from.
    ///
    /// Batches are grouped by material *within* a model and never across one,
    /// because a body part is a set of alternatives of which only one is drawn.
    /// Body groups are near-vestigial on static props — 959 of 968 models have
    /// exactly one body part, and every body part exactly one model — but
    /// merging across them would make body-group selection impossible to add.
    pub body_part: u16,
    pub model: u16,
}

/// One bone's slice of a [`Batch`]'s indices.
///
/// The triangles a batch draws are sorted by bone at load, so each bone's are
/// contiguous and can be drawn as one range under that bone's matrix. That is
/// this port's substitute for skinning — see [`StudioModel::rigid_bones`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoneRun {
    pub bone: u16,
    /// Offset into [`StudioModel::indices`], not into the batch.
    pub first_index: u32,
    pub index_count: u32,
}

/// A studio model, resolved and ready to upload.
///
/// One vertex buffer and one index buffer for the whole model, sliced into
/// [`Batch`]es by material. That is the layout `r_studiodraw.cpp` builds too,
/// for the same reason: a mesh's vertices are contiguous in the `.vvd` and its
/// indices are contiguous in the `.vtx`, so no gather is needed.
#[derive(Debug, Clone)]
pub struct StudioModel {
    /// The path this was loaded from, e.g. `models/props_bts/gantry_rails_a.mdl`.
    pub path: String,
    /// `studiohdr_t::name` — the name the *compiler* recorded, which is not
    /// always the path it ships at.
    pub name: String,
    /// The **render** bounds, in model space — `Mdl::render_bounds`, which is
    /// `view_bbmin`/`view_bbmax` when the compiler wrote them and
    /// `hull_min`/`hull_max` when it did not.
    ///
    /// **Not `view_bbmin`/`view_bbmax` straight.** Most props ship with both
    /// zero, so reading them raw gives a degenerate box at the model's origin
    /// — a cull box that drops the prop from most viewpoints. See
    /// [`Mdl::render_bounds`](mdl::Mdl::render_bounds) for Valve's three
    /// spellings of the same fallback.
    pub bounds: (Vec3, Vec3),
    /// `illumposition` — where lighting is sampled when the placing entity
    /// names no lighting origin of its own.
    pub illum_position: Vec3,
    pub flags: StudioFlags,
    /// Shared by all three files; also what a `.vhv` must match.
    pub checksum: u32,
    pub vertices: Vec<ModelVertex>,
    pub indices: Vec<u32>,
    pub batches: Vec<Batch>,
    /// The studio meshes of LOD 0, in file order — what a [`Vhv`]'s meshes are
    /// matched against.
    pub meshes: Vec<HardwareMesh>,
    /// The bone list. Empty for a model with no bones at all.
    pub bones: Vec<anim::Bone>,
    /// The sequences, in file order. [`sequence`](StudioModel::sequence) is
    /// `LookupSequence`.
    pub sequences: Vec<anim::Sequence>,
    /// The animations, parallel to nothing — a [`Sequence`](anim::Sequence)
    /// indexes them.
    pub animations: Vec<anim::Animation>,
    /// The `$includemodel` companions whose sequences and animations are in
    /// the two lists above, in the order they were merged.
    ///
    /// Empty for all but nine of the 606 models the game's `prop_dynamic`s
    /// name — and those nine are worn by 926 entities, so it is empty for
    /// most models and load-bearing for a lot of props. See
    /// [`include`](self::include) for what merging one does; a model that
    /// *declares* an include which could not be read does not list it here.
    pub includes: Vec<String>,
    /// Which bone moves each vertex, parallel to
    /// [`vertices`](StudioModel::vertices).
    ///
    /// `None` when some vertex is moved by more than one — see
    /// [`rigid_bones`](StudioModel::rigid_bones).
    vertex_bones: Option<Vec<u8>>,
}

/// One studio mesh, as the *hardware* would hold it.
///
/// Kept beside the [`Batch`]es rather than folded into them because they answer
/// different questions: a batch is "what to draw with which material", and this
/// is "which vertex of the buffer is this mesh's `n`th" — the only thing that
/// can line a `.vhv`'s per-mesh colour blocks up with the vertex buffer.
/// Batches merge meshes that share a material and skip empty ones, so they
/// cannot do it.
///
/// **This is not a range.** Valve's runtime compacts a model's vertices per LOD
/// — `studiomeshgroup_t`'s buffer holds exactly the vertices that LOD's strips
/// reference, in the order the `.vtx` strip-group tables list them — and a
/// `.vhv` is written against *that* numbering, which is what "hardware verts"
/// means. This port does not compact (it uploads the whole `.vvd` pool and
/// indexes into it), so the two numberings differ whenever a lower LOD uses a
/// subset of the pool: `models/props_destruction/framework_dest_01` has 9,434
/// pool vertices and 6,703 hardware vertices at LOD 0. Treating the `.vhv` as a
/// prefix of the pool lights 125 of `sp_a1_intro1`'s 1,080 props from the wrong
/// vertices — and, worse, silently *appears* to work for the other 955,
/// because a single-LOD model's table is usually the identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareMesh {
    /// Where each hardware vertex lives in [`StudioModel::vertices`], in the
    /// order a `.vhv` block is written.
    pub vertices: Vec<u32>,
}

impl StudioModel {
    /// Reads `<name>.mdl` and its two companions.
    ///
    /// `name` may carry the `.mdl` extension or not; both
    /// `models/props_bts/gantry_rails_a` and `...gantry_rails_a.mdl` work, as
    /// they do in the original, where the `sprp` dictionary stores the
    /// extension and most other callers do not.
    pub fn load(vfs: &Vfs, name: &str) -> Result<StudioModel, StudioError> {
        let stem = name
            .strip_suffix(".mdl")
            .or_else(|| name.strip_suffix(".MDL"))
            .unwrap_or(name)
            .replace('\\', "/")
            .to_ascii_lowercase();

        let read = |path: &str| -> Result<Vec<u8>, StudioError> {
            vfs.read(path).map_err(|source| StudioError::Read {
                path: path.to_owned(),
                source,
            })
        };

        let mdl_path = format!("{stem}.mdl");
        let vvd_path = format!("{stem}.vvd");
        let vtx_path = format!("{stem}{VTX_EXTENSION}");

        let mut mdl = Mdl::parse(mdl_path.clone(), &read(&mdl_path)?)?;

        // `CStudioHdr::ResolveIncludedModels`, before anything reads the
        // sequence list. A model that names no `$includemodel` — 2,160 of the
        // game's 2,186 — reads no extra file and this is one empty loop.
        let includes = include::resolve(&mut mdl, |path| vfs.read(path).ok());

        let vvd_bytes = read(&vvd_path)?;
        let vvd = Vvd::parse(vvd_path, &vvd_bytes)?;
        let vtx_bytes = read(&vtx_path)?;
        let vtx = Vtx::parse(vtx_path, &vtx_bytes)?;

        let mut model = build::build(&mdl, &vvd, &vtx, |candidates| {
            candidates
                .iter()
                .find(|candidate| vfs.exists(&format!("materials/{candidate}.vmt")))
                .cloned()
        })?;
        // `build` joins three *files* and knows nothing about a fourth, so the
        // list is attached here rather than threaded through it.
        model.includes = includes;
        Ok(model)
    }

    /// Total triangles across every batch.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// `LookupSequence` (`studio.cpp`) — a sequence's index by label, case
    /// insensitively.
    pub fn sequence(&self, label: &str) -> Option<usize> {
        self.sequences
            .iter()
            .position(|s| s.label.eq_ignore_ascii_case(label))
    }

    /// The animation a sequence plays, if the sequence exists.
    pub fn animation(&self, sequence: usize) -> Option<&anim::Animation> {
        let sequence = self.sequences.get(sequence)?;
        self.animations.get(sequence.anim)
    }

    /// Which bone moves each vertex — `Some` only if **every** vertex is moved
    /// by exactly one.
    ///
    /// # Why this decides how a model is drawn
    ///
    /// Valve skins: every vertex is transformed on the way to the GPU by a
    /// weighted blend of up to three bone matrices. This port does not, and
    /// draws each bone's triangles as their own range under that bone's matrix
    /// instead — which needs no change to the vertex format, no bone matrices
    /// on the GPU and no change to any shader, and is **exact** for a model
    /// whose vertices each answer to one bone.
    ///
    /// That is a measurement, not a hope — and it is a *bounded* one. Every
    /// vertex of all four floor-button models binds to exactly one bone, and a
    /// `prop_floor_button`'s 7,929 split 7,263 on the body and 666 on the
    /// plate. But across the whole game **420 of 2,017 models have more than
    /// one bone and 141 of those share a vertex between two**, so the split is
    /// exact for 279 of them and not for the rest — the `a4_destruction` set,
    /// which is Wheatley's chamber falling apart.
    ///
    /// So `None` is not hypothetical: it is the 141, and it is the measured
    /// condition that makes real skinning worth writing. A model that returns
    /// it is drawn in its **bind pose** rather than wrongly, and counted
    /// (`EntityModelStats::models_not_rigid`).
    pub fn rigid_bones(&self) -> Option<&[u8]> {
        self.vertex_bones.as_deref()
    }
}

/// Joins three already-parsed files, resolving materials through `resolve`.
///
/// Split out of [`StudioModel::load`] so the join can be tested against
/// synthetic files with no `Vfs` — `resolve` is given the candidate paths for
/// one texture slot, in the order the `.mdl` wants them tried, and returns the
/// one that exists.
pub fn assemble(
    mdl: &Mdl,
    vvd: &Vvd,
    vtx: &Vtx,
    resolve: impl Fn(&[String]) -> Option<String>,
) -> Result<StudioModel, StudioError> {
    build::build(mdl, vvd, vtx, resolve)
}

#[cfg(test)]
mod tests {
    use super::fixture::{pool_position, MeshSpec, ModelSpec, Spec};
    use super::*;

    /// Parses a spec's three files and joins them, resolving any material whose
    /// candidate path starts with `models/test/`.
    fn assemble_spec(spec: &Spec) -> Result<StudioModel, StudioError> {
        let (mdl_bytes, vvd_bytes, vtx_bytes) = spec.build();
        let mdl = Mdl::parse("test.mdl".to_owned(), &mdl_bytes)?;
        let vvd = Vvd::parse("test.vvd".to_owned(), &vvd_bytes)?;
        let vtx = Vtx::parse("test.dx90.vtx".to_owned(), &vtx_bytes)?;
        assemble(&mdl, &vvd, &vtx, |candidates| {
            candidates
                .iter()
                .find(|c| c.starts_with("models/test/"))
                .cloned()
        })
    }

    /// The struct strides the three formats are read at.
    ///
    /// These are the numbers that turn a correct reader into a garbage one
    /// without any error: a wrong stride walks off into the middle of the next
    /// record and reads plausible nonsense. Verified against all 2,041 models
    /// in the shipped game, which is where the confidence comes from — see
    /// `portdocs/STUDIO.md` §4.
    #[test]
    fn struct_strides_match_the_shipped_files() {
        assert_eq!(super::mdl::MODEL_STRIDE, 148, "mstudiomodel_t");
        assert_eq!(super::mdl::MESH_STRIDE, 116, "mstudiomesh_t");
    }

    #[test]
    fn a_minimal_model_parses_and_joins() {
        let model = assemble_spec(&Spec::default()).expect("a well-formed trio");
        assert_eq!(model.vertices.len(), 3);
        assert_eq!(model.indices, vec![0, 1, 2]);
        assert_eq!(model.batches.len(), 1);
        assert_eq!(model.batches[0].material, "models/test/wall");
        assert_eq!(model.triangle_count(), 1);
    }

    /// `origMeshVertID` is relative to the *mesh*, so a second mesh's triangles
    /// must land on that mesh's vertices and not the first mesh's.
    ///
    /// Dropping `mesh.vertex_offset` makes both meshes draw the first one's
    /// geometry, which looks like a broken model rather than a broken reader —
    /// see [`build`](super::build).
    #[test]
    fn a_mesh_vertex_offset_shifts_its_indices() {
        let spec = Spec {
            pool_vertices: 6,
            body_parts: vec![ModelSpec {
                vertex_index: 0,
                vertex_count: 6,
                meshes: vec![
                    MeshSpec {
                        material: 0,
                        vertex_offset: 0,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    },
                    MeshSpec {
                        material: 0,
                        vertex_offset: 3,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    },
                ],
            }],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("a well-formed trio");
        // Both meshes name 0,1,2 in the file; the second resolves to 3,4,5.
        assert_eq!(model.indices, vec![0, 1, 2, 3, 4, 5]);
    }

    /// `mstudiomodel_t::vertexindex` is a byte offset into the pool, and the
    /// model's base has to be added on top of the mesh's.
    #[test]
    fn a_model_base_offsets_into_the_pool() {
        let spec = Spec {
            pool_vertices: 9,
            body_parts: vec![
                ModelSpec {
                    vertex_index: 0,
                    vertex_count: 3,
                    meshes: vec![MeshSpec {
                        material: 0,
                        vertex_offset: 0,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    }],
                },
                ModelSpec {
                    vertex_index: 6,
                    vertex_count: 3,
                    meshes: vec![MeshSpec {
                        material: 0,
                        vertex_offset: 0,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    }],
                },
            ],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("a well-formed trio");
        assert_eq!(model.indices, vec![0, 1, 2, 6, 7, 8]);
        assert_eq!(model.batches.len(), 2, "one per body part, never merged");
        assert_eq!(model.batches[1].body_part, 1);
    }

    /// The fixup table's whole purpose: the pool is stored LOD-sorted and the
    /// table puts it back into mesh order.
    ///
    /// Only 15 of Portal 2's 968 static prop models have a table, and 14 of
    /// those are a genuine permutation at root LOD 0 — so skipping this leaves
    /// 954 models perfect and 14 scrambled.
    #[test]
    fn the_fixup_table_reorders_the_vertex_pool() {
        let spec = Spec {
            pool_vertices: 6,
            // Two runs, swapped: the second half of the pool comes first.
            fixups: vec![(0, 3, 3), (0, 0, 3)],
            lod_vertex_counts: Some([6; 8]),
            body_parts: vec![ModelSpec {
                vertex_index: 0,
                vertex_count: 6,
                meshes: vec![MeshSpec {
                    material: 0,
                    vertex_offset: 0,
                    vertex_count: 6,
                    triangles: vec![[0, 1, 2], [3, 4, 5]],
                }],
            }],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("a well-formed trio");
        assert_eq!(model.vertices.len(), 6);
        // Output slot 0 holds pool vertex 3, and slot 3 holds pool vertex 0.
        assert_eq!(model.vertices[0].position, pool_position(3));
        assert_eq!(model.vertices[3].position, pool_position(0));
    }

    /// The tangent array is permuted by the same table, in lockstep. Permuting
    /// one and not the other gives correct silhouettes with wrong lighting.
    #[test]
    fn fixups_permute_the_tangents_with_the_vertices() {
        let spec = Spec {
            pool_vertices: 6,
            fixups: vec![(0, 3, 3), (0, 0, 3)],
            lod_vertex_counts: Some([6; 8]),
            body_parts: vec![ModelSpec {
                vertex_index: 0,
                vertex_count: 6,
                meshes: vec![MeshSpec {
                    material: 0,
                    vertex_offset: 0,
                    vertex_count: 6,
                    triangles: vec![[0, 1, 2], [3, 4, 5]],
                }],
            }],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("a well-formed trio");
        // The fixture stores the pool index in the tangent's `w`.
        assert_eq!(model.vertices[0].tangent[3], 3.0);
        assert_eq!(model.vertices[3].tangent[3], 0.0);
    }

    /// A table whose runs do not add up to the header's LOD vertex count means
    /// the two halves of the file disagree, and the `.vtx`'s indices are
    /// measured against a length the pool does not have.
    #[test]
    fn a_fixup_table_that_disagrees_with_the_header_is_refused() {
        let spec = Spec {
            pool_vertices: 6,
            fixups: vec![(0, 0, 3)],
            lod_vertex_counts: Some([6; 8]),
            ..Spec::default()
        };
        let (_, vvd_bytes, _) = spec.build();
        let err = Vvd::parse("test.vvd".to_owned(), &vvd_bytes).unwrap_err();
        assert!(
            matches!(err, StudioError::Corrupt { .. }),
            "expected Corrupt, got {err:?}"
        );
    }

    /// Material resolution is a cross product over `cdtextures`, first hit
    /// wins — not a filesystem search path.
    #[test]
    fn materials_resolve_through_the_cdtexture_cross_product() {
        let spec = Spec {
            texture_dirs: vec!["models/other".to_owned(), "models/test".to_owned()],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("a well-formed trio");
        // Only the second directory exists to the resolver, so it wins even
        // though the first is tried first.
        assert_eq!(model.batches[0].material, "models/test/wall");
    }

    /// 8 texture references across the whole shipped game resolve to nothing.
    /// A model with one still loads — the batch keeps the first candidate, so
    /// `MaterialCache` answers it with the error material.
    #[test]
    fn an_unresolvable_material_keeps_its_first_candidate() {
        let spec = Spec {
            texture_dirs: vec!["models/nowhere".to_owned()],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("an unresolvable material is not a load failure");
        assert_eq!(model.batches[0].material, "models/nowhere/wall");
    }

    /// Meshes sharing a material become one batch; meshes that do not stay
    /// separate.
    #[test]
    fn batches_group_by_material_within_a_model() {
        let spec = Spec {
            textures: vec!["wall".to_owned(), "floor".to_owned()],
            pool_vertices: 9,
            body_parts: vec![ModelSpec {
                vertex_index: 0,
                vertex_count: 9,
                meshes: vec![
                    MeshSpec {
                        material: 0,
                        vertex_offset: 0,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    },
                    MeshSpec {
                        material: 1,
                        vertex_offset: 3,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    },
                    MeshSpec {
                        material: 0,
                        vertex_offset: 6,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    },
                ],
            }],
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("a well-formed trio");
        assert_eq!(model.batches.len(), 2, "two materials, three meshes");
        let wall = &model.batches[0];
        assert_eq!(wall.material, "models/test/wall");
        assert_eq!(wall.index_count, 6, "both wall meshes in one batch");
        let indices = &model.indices[wall.first_index as usize..][..6];
        assert_eq!(indices, [0, 1, 2, 6, 7, 8]);
    }

    /// A `.vtx` from a different build of the model parses fine and indexes the
    /// wrong vertices, so the checksum is the only cheap way to catch it.
    #[test]
    fn a_stale_companion_file_is_refused() {
        let spec = Spec {
            vtx_checksum: Some(0xdead_beef),
            ..Spec::default()
        };
        let err = assemble_spec(&spec).unwrap_err();
        assert!(
            matches!(err, StudioError::ChecksumMismatch { .. }),
            "expected ChecksumMismatch, got {err:?}"
        );
    }

    #[test]
    fn each_file_refuses_a_version_it_does_not_read() {
        for spec in [
            Spec {
                mdl_version: 48,
                ..Spec::default()
            },
            Spec {
                vvd_version: 3,
                ..Spec::default()
            },
            Spec {
                vtx_version: 6,
                ..Spec::default()
            },
        ] {
            let err = assemble_spec(&spec).unwrap_err();
            assert!(
                matches!(err, StudioError::Version { .. }),
                "expected Version, got {err:?}"
            );
        }
    }

    /// No static prop in Portal 2 has either, and reading one as triangles
    /// would draw nonsense rather than fail.
    #[test]
    fn quad_lists_and_flex_deltas_are_refused() {
        for spec in [
            Spec {
                strip_flags: 0x02, // STRIP_IS_QUADLIST_REG
                ..Spec::default()
            },
            Spec {
                strip_group_flags: 0x02 | 0x04, // ..._IS_DELTA_FLEXED
                ..Spec::default()
            },
        ] {
            let err = assemble_spec(&spec).unwrap_err();
            assert!(
                matches!(err, StudioError::Corrupt { .. }),
                "expected Corrupt, got {err:?}"
            );
        }
    }

    /// One shipped strip carries flags 0 rather than `STRIP_IS_TRILIST`. It is
    /// a trilist; the flag is simply unset, and refusing it would drop a real
    /// model's geometry.
    #[test]
    fn a_strip_with_no_flags_is_read_as_a_triangle_list() {
        let spec = Spec {
            strip_flags: 0,
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("an unflagged strip is a trilist");
        assert_eq!(model.indices, vec![0, 1, 2]);
    }

    /// `zip` would silently pair the wrong body parts and drop the rest, so the
    /// shapes are compared before anything is joined.
    #[test]
    fn a_vtx_that_disagrees_about_shape_is_refused() {
        let two = Spec {
            pool_vertices: 6,
            body_parts: vec![
                ModelSpec {
                    vertex_index: 0,
                    vertex_count: 3,
                    meshes: vec![MeshSpec {
                        material: 0,
                        vertex_offset: 0,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    }],
                },
                ModelSpec {
                    vertex_index: 3,
                    vertex_count: 3,
                    meshes: vec![MeshSpec {
                        material: 0,
                        vertex_offset: 0,
                        vertex_count: 3,
                        triangles: vec![[0, 1, 2]],
                    }],
                },
            ],
            ..Spec::default()
        };
        let one = Spec::default();

        let (mdl_bytes, vvd_bytes, _) = two.build();
        let (_, _, vtx_bytes) = one.build();
        let mdl = Mdl::parse("test.mdl".to_owned(), &mdl_bytes).expect("a well-formed .mdl");
        let vvd = Vvd::parse("test.vvd".to_owned(), &vvd_bytes).expect("a well-formed .vvd");
        let vtx = Vtx::parse("test.dx90.vtx".to_owned(), &vtx_bytes).expect("a well-formed .vtx");

        let err = assemble(&mdl, &vvd, &vtx, |_| None).unwrap_err();
        assert!(
            matches!(err, StudioError::Corrupt { .. }),
            "expected Corrupt, got {err:?}"
        );
    }

    /// `vertexindex` counts bytes, so an offset that is not a whole number of
    /// 48-byte vertices would shear every vertex in the model.
    #[test]
    fn a_vertexindex_that_is_not_a_whole_vertex_is_refused() {
        let (mut mdl_bytes, _, _) = Spec::default().build();
        // Find the model and corrupt its `vertexindex` by one byte.
        let body_part_base = i32::from_le_bytes(mdl_bytes[236..240].try_into().unwrap()) as usize;
        let model_at = body_part_base
            + (i32::from_le_bytes(
                mdl_bytes[body_part_base + 12..body_part_base + 16]
                    .try_into()
                    .unwrap(),
            ) as usize);
        mdl_bytes[model_at + 84..model_at + 88].copy_from_slice(&1i32.to_le_bytes());

        let err = Mdl::parse("test.mdl".to_owned(), &mdl_bytes).unwrap_err();
        assert!(
            matches!(err, StudioError::Corrupt { .. }),
            "expected Corrupt, got {err:?}"
        );
    }

    /// A model with no tangent block still produces usable vertices: a zero
    /// `w` would mirror every bumped surface's lighting along V.
    #[test]
    fn a_missing_tangent_block_yields_a_real_tangent() {
        let spec = Spec {
            tangents: false,
            ..Spec::default()
        };
        let model = assemble_spec(&spec).expect("tangents are optional");
        assert_eq!(model.vertices[0].tangent, [1.0, 0.0, 0.0, 1.0]);
    }

    /// A model's geometry carries no baked light at all.
    ///
    /// It cannot: `vrad` bakes one colour per vertex **per placement**, and a
    /// `StudioModel` is the asset every placement shares. The light arrives as
    /// a second vertex stream that `engine::world::props` fills per instance —
    /// see `materials::mesh::StaticLightVertex`. This test exists so that
    /// putting it back here fails rather than quietly lighting a thousand props
    /// identically.
    #[test]
    fn a_model_carries_no_baked_light_because_it_is_per_placement() {
        let model = assemble_spec(&Spec::default()).expect("a well-formed trio");
        assert_eq!(
            size_of::<ModelVertex>(),
            48,
            "position, normal, texcoord and tangent — and no colour"
        );
        assert_eq!(model.vertices.len(), 3);
    }

    /// Every studio model the shipped game holds, parsed for real.
    ///
    /// Ignored by default and gated on `KISAK_GAME_DIR`, because the depot is
    /// not in this repository and the other 570 tests deliberately need no game
    /// files. Run it against a real install with:
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release studio_models -- --ignored --nocapture
    /// ```
    ///
    /// This is the verification `portdocs/STUDIO.md` §8 stage 1 asks for, and
    /// it is a strong test precisely because §3 already measured the answers:
    /// a wrong struct stride or a missed indirection does not fail quietly here
    /// — it walks off the end of a lump and the range checks in
    /// [`build`](super::build) catch it.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_studio_model_parses() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        // Walk `models/` for every `.mdl`. `Vfs::list` merges the mounts, so
        // this sees loose files and VPK entries alike.
        let mut stack = vec!["models".to_owned()];
        let mut paths = Vec::new();
        while let Some(at) = stack.pop() {
            let Ok(entries) = vfs.list(&at) else { continue };
            for entry in entries {
                let child = format!("{at}/{}", entry.name);
                if entry.is_dir {
                    stack.push(child);
                } else if child.to_ascii_lowercase().ends_with(".mdl") {
                    paths.push(child);
                }
            }
        }
        paths.sort();
        assert!(
            paths.len() > 1_000,
            "only {} models found under models/ — is this a Portal 2 install?",
            paths.len()
        );

        let (mut loaded, mut static_props, mut skipped) = (0, 0, 0);
        // The measurement the whole animation path rests on: how many models
        // in the game have a vertex that answers to more than one bone, and so
        // could not be posed by splitting their triangles between bones.
        let (mut multi_bone, mut not_rigid) = (0usize, Vec::new());
        let (mut sequences, mut widest_animation) = (0usize, 0usize);
        // `$includemodel`: how many models declare one, how many of those the
        // game actually ships, and what merging them is worth.
        let (mut declares, mut merged, mut from_includes) = (0usize, 0usize, 0usize);
        // `Mdl::render_bounds`' reason to exist.
        let (mut no_view_bb, mut rescued) = (0usize, 0usize);
        let (mut widest_hull, mut widest_hull_at) = (0.0f32, String::new());
        let (mut outside, mut raw_outside, mut outside_static) = (0usize, 0usize, 0usize);
        let (mut worst_outside, mut worst_raw) = (0.0f32, 0.0f32);
        let mut worst_outside_at = String::new();
        let (mut posed_worst, mut posed_worst_at) = (0.0f32, String::new());
        let (mut seq_reach, mut seq_nonzero) = (0.0f32, 0usize);
        let mut posed_over: std::collections::HashMap<String, f32> =
            std::collections::HashMap::new();
        let (mut widest, mut widest_at) = (0usize, String::new());
        let (mut failed, mut animated_failed) = (Vec::new(), Vec::new());
        for path in &paths {
            let stem = path.trim_end_matches(".mdl");
            // A model whose companions are absent is not this reader's problem
            // — the game ships `.mdl`-only entries for things it never draws.
            if !vfs.exists(&format!("{stem}.vvd")) || !vfs.exists(&format!("{stem}{VTX_EXTENSION}"))
            {
                skipped += 1;
                continue;
            }

            // The `.mdl` alone decides whether this reader is *supposed* to
            // cope: §3's measurements are about static props, and an animated
            // model may legitimately carry flex deltas or sub-d patches that
            // this reader refuses on purpose. So the flag is read before the
            // whole trio is asked for, and only a static prop's failure is a
            // failure of the port.
            //
            // It is also where `$includemodel` is counted, because this is the
            // model *before* its companions are merged in: how many sequences
            // it owns, against how many it ends up with.
            let header = Mdl::parse(path.clone(), &vfs.read(path).expect("read the .mdl")).ok();
            let is_static = header
                .as_ref()
                .map(|mdl| mdl.flags.contains(StudioFlags::STATIC_PROP))
                .unwrap_or(false);
            let (declared, local_sequences) = header
                .as_ref()
                .map(|mdl| (mdl.include_models.len(), mdl.sequences.len()))
                .unwrap_or((0, 0));
            declares += usize::from(declared > 0);
            // **The assumption `world::props::models` makes about a prop's two
            // lighting sources.** They replace each other — the colour mesh or
            // the light cache, never both — and this flag is the one thing that
            // would make them add. Valve sets it at load, from cvars Portal 2
            // leaves at their defaults; this checks no shipped *file* carries
            // it either.
            if let Some(mdl) = &header {
                assert!(
                    !mdl.flags
                        .contains(StudioFlags::BAKED_VERTEX_LIGHTING_IS_INDIRECT_ONLY),
                    "{path} asks for indirect-only baked lighting"
                );

                // **How often `view_bbmin`/`view_bbmax` is zero**, which is
                // the measurement behind `Mdl::render_bounds`. A model with
                // both zero has no clipping box, and reading it raw gives a
                // degenerate cull box at the origin — the bug that made
                // `prop_dynamic`s vanish at some angles and not others.
                if mdl.bounds.0 == Vec3::ZERO && mdl.bounds.1 == Vec3::ZERO {
                    no_view_bb += 1;
                    let (mins, maxs) = mdl.hull;
                    let reach = mins.abs().max(maxs.abs()).max_element();
                    if reach > 0.0 {
                        rescued += 1;
                    }
                    if reach > widest_hull {
                        widest_hull = reach;
                        widest_hull_at = path.clone();
                    }
                }
                assert_eq!(
                    mdl.render_bounds(),
                    match mdl.bounds == (Vec3::ZERO, Vec3::ZERO) {
                        true => mdl.hull,
                        false => mdl.bounds,
                    },
                    "{path}: render_bounds is not the fallback"
                );
            }

            match StudioModel::load(&vfs, path) {
                Ok(model) => {
                    loaded += 1;
                    // **Does the cull box contain what is drawn?** The whole
                    // point of `render_bounds`, measured against the geometry
                    // rather than against another header field.
                    let mut lo = Vec3::splat(f32::MAX);
                    let mut hi = Vec3::splat(f32::MIN);
                    for v in &model.vertices {
                        let p = Vec3::from_array(v.position);
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                    if !model.vertices.is_empty() {
                        let out = (model.bounds.0 - lo).max(hi - model.bounds.1).max_element();
                        if out > 0.01 {
                            outside += 1;
                            if is_static && out > worst_outside {
                                worst_outside = out;
                                worst_outside_at = path.clone();
                            }
                            if is_static {
                                outside_static += 1;
                            }
                        }
                        // **And the same question for every pose the model
                        // can be drawn in**, which is the one that matters for
                        // `prop_dynamic`: a bind-pose vertex outside the hull
                        // is not necessarily drawn there, and a posed one is.
                        // Drawn exactly the way `EntityModels` draws it —
                        // per-vertex bone when the model is rigid, bind pose
                        // when it is not.
                        if let Some(vertex_bones) = model.rigid_bones() {
                            for sequence in 0..model.sequences.len() {
                                // `STUDIO_DELTA`: the sequence *adds* to a
                                // base pose rather than replacing it, so both
                                // its animation and its declared box are
                                // deltas. Posing one standalone is
                                // meaningless, and nothing in this port plays
                                // one — `prop_dynamic` names a sequence by
                                // label and layers nothing on top.
                                if model.sequences[sequence].flags & 0x0004 != 0 {
                                    continue;
                                }
                                for step in 0..3 {
                                    let bones = anim::pose(
                                        &model.bones,
                                        model.animation(sequence),
                                        step as f32 * 0.5,
                                    );
                                    let mut plo = Vec3::splat(f32::MAX);
                                    let mut phi = Vec3::splat(f32::MIN);
                                    for (i, v) in model.vertices.iter().enumerate() {
                                        let bone = bones
                                            .get(usize::from(vertex_bones[i]))
                                            .copied()
                                            .unwrap_or(glam::Mat4::IDENTITY);
                                        let p = bone
                                            .transform_point3(Vec3::from_array(v.position));
                                        plo = plo.min(p);
                                        phi = phi.max(p);
                                    }
                                    // The cull box `EntityModels` actually
                                    // uses: the render bounds merged with
                                    // this sequence's own box.
                                    let seq = model.sequences[sequence].bounds;
                                    let lo = model.bounds.0.min(seq.0);
                                    let hi = model.bounds.1.max(seq.1);
                                    let out = (lo - plo).max(phi - hi).max_element();
                                    if out > 1.0 {
                                        posed_over
                                            .entry(path.clone())
                                            .and_modify(|w| *w = out.max(*w))
                                            .or_insert(out);
                                    }
                                    if out > posed_worst {
                                        posed_worst = out;
                                        posed_worst_at =
                                            format!("{path} sequence {sequence}");
                                    }
                                }
                            }
                        }

                        // What it would have been without the fallback: the
                        // number that says how bad the bug was.
                        let raw = header
                            .as_ref()
                            .map(|mdl| mdl.bounds)
                            .unwrap_or(model.bounds);
                        let raw_out = (raw.0 - lo).max(hi - raw.1).max_element();
                        worst_raw = worst_raw.max(raw_out);
                        if raw_out > 0.01 {
                            raw_outside += 1;
                        }
                    }
                    sequences += model.sequences.len();
                    // A sanity check on the two new header reads: a sequence
                    // box outside the biggest map the engine allows is a
                    // misread offset, not content.
                    for seq in &model.sequences {
                        let reach = seq
                            .bounds
                            .0
                            .abs()
                            .max(seq.bounds.1.abs())
                            .max_element();
                        assert!(
                            reach.is_finite() && reach < 65_536.0,
                            "{path}: sequence {} has bounds {:?}",
                            seq.label,
                            seq.bounds
                        );
                        seq_reach = seq_reach.max(reach);
                        seq_nonzero += usize::from(seq.bounds != (Vec3::ZERO, Vec3::ZERO));
                    }
                    merged += model.includes.len();
                    from_includes += model.sequences.len() - local_sequences;
                    widest_animation = widest_animation.max(
                        model
                            .animations
                            .iter()
                            .map(|a| a.frame_count)
                            .max()
                            .unwrap_or(0),
                    );
                    if model.bones.len() > 1 {
                        multi_bone += 1;
                        if model.rigid_bones().is_none() {
                            not_rigid.push(path.clone());
                        }
                    }
                    if is_static && model.vertices.len() > widest {
                        widest = model.vertices.len();
                        widest_at = path.clone();
                    }
                    if is_static {
                        static_props += 1;
                        // §3's measurements, re-asserted against the data.
                        assert_eq!(
                            model.indices.len() % 3,
                            0,
                            "{path} does not hold whole triangles"
                        );
                        assert!(!model.vertices.is_empty(), "{path} has no vertices");
                        let covered: u32 = model.batches.iter().map(|b| b.index_count).sum();
                        assert_eq!(
                            covered as usize,
                            model.indices.len(),
                            "{path}'s batches do not cover its indices"
                        );
                    }
                }
                Err(e) if is_static => failed.push(format!("{path}: {e}")),
                Err(e) => animated_failed.push(format!("{path}: {e}")),
            }
        }

        println!(
            "{} models under models/: {loaded} loaded ({static_props} static props), \
             {skipped} without companions, {} static props failed, \
             {} non-static models refused",
            paths.len(),
            failed.len(),
            animated_failed.len()
        );
        println!("widest static prop: {widest} vertices ({widest_at})");
        println!(
            "{multi_bone} of {loaded} models have more than one bone; \
             {} of those share a vertex between bones; \
             {sequences} sequences, longest animation {widest_animation} frames",
            not_rigid.len()
        );
        println!(
            "{declares} models declare a $includemodel; {merged} include(s) merged, \
             carrying {from_includes} of the {sequences} sequences"
        );
        println!(
            "{no_view_bb} models have no view bbox; {rescued} of those have a hull \
             (widest reach {widest_hull:.0} units, {widest_hull_at})"
        );
        println!(
            "cull box vs. geometry: {outside} of {loaded} models reach outside their \
             render bounds, {outside_static} of them static props \
             (worst static {worst_outside:.0} units, {worst_outside_at}); \
             reading view_bbmin raw it would be {raw_outside}, worst {worst_raw:.0}"
        );
        println!(
            "{seq_nonzero} of {sequences} sequences declare a box; widest reach \
             {seq_reach:.0} units"
        );
        {
            let mut over: Vec<(&String, &f32)> = posed_over.iter().collect();
            over.sort_by(|a, b| b.1.partial_cmp(a.1).expect("finite"));
            println!(
                "{} models pose outside their cull box (worst {posed_worst:.0} units, \
                 {posed_worst_at})",
                over.len()
            );
            for (path, out) in over.iter().take(10) {
                println!("  {out:>8.0}  {path}");
            }
        }
        // **The fallback is the common case, not the corner case.** If this
        // ever stopped holding, `Mdl::render_bounds` would look like dead
        // defensive code and somebody would delete it.
        assert!(
            no_view_bb * 2 > loaded,
            "only {no_view_bb} of {loaded} models lack a view bbox — \
             Mdl::render_bounds' fallback no longer carries the weight it was written for"
        );
        // **What the fallback buys, as an invariant rather than a count**: a
        // static prop is drawn in its bind pose and nothing else, so its
        // render bounds must contain its geometry exactly. Before the
        // fallback every one of the 1,444 failed this.
        assert_eq!(
            outside_static, 0,
            "{outside_static} static props reach outside their render bounds"
        );
        assert!(
            widest_hull > 64.0,
            "the widest hull of a model with no view bbox is {widest_hull} units; \
             a degenerate cull box would have been hidden by the pad \
             `engine::world::entities::cull_box` adds"
        );
        for line in not_rigid.iter().take(10) {
            println!("  shares vertices between bones: {line}");
        }
        for line in animated_failed.iter().take(5) {
            println!("  (not a static prop) {line}");
        }
        for line in failed.iter().take(20) {
            println!("  {line}");
        }
        assert!(
            failed.is_empty(),
            "{} static props failed to load",
            failed.len()
        );

        // **The two numbers the animation path's shape rests on, and both of
        // them bound it rather than bless it.**
        //
        // 420 models have a skeleton worth posing and **141 of those share a
        // vertex between bones**, so the per-bone draw split
        // ([`StudioModel::rigid_bones`]) is exact for 279 of them and not for
        // the rest. That is fine today — every model an *entity* places is in
        // the 279 — and it is the measurement that says real skinning is worth
        // writing the moment something places one of the 141. They are the
        // `a4_destruction` set, which is Wheatley's chamber falling apart.
        //
        // And the longest animation in the game is **4,050 frames**, against
        // the 11 a floor button's has. `anim.rs` expands every animation at
        // load; at 4,050 frames that is a megabyte or so for one model, which
        // is affordable but is the number to watch if a class ever loads one.
        // **`$includemodel`, measured.** 25 of the models this mount can see
        // name one, and 24 of those companions can be read: the three that
        // cannot are all `models/props_lab/bot_male.mdl`'s —
        // `bot_male_animations`, `_gestures` and `_postures` are in no VPK —
        // so that model keeps its seven local sequences and nothing else,
        // which is `FindModel` returning null and is not an error.
        //
        // (A 26th host, `models/info_character/info_character_player.mdl`,
        // lives in `portal2_dlc2`, which `portal2/gameinfo.txt` does not
        // mount. It is not in the 2,041 above either.)
        //
        // Nine of them are placed by a `prop_dynamic`, by **926 entities**,
        // and merging their companions is what turns 270 `DefaultAnim` labels
        // — 849 entities' worth — from "no such sequence" into a pose.
        assert_eq!(declares, 25, "models that name a $includemodel");
        assert_eq!(merged, 24, "companions that could be read");
        assert_eq!(multi_bone, 420, "models with more than one bone");
        assert_eq!(
            not_rigid.len(),
            141,
            "models that share a vertex between bones"
        );
        // Was 5,434 before the merge, and the difference is what every
        // `prop_dynamic` naming an `anim_wp/room_transform` sequence was
        // missing.
        assert_eq!(sequences, 10_666, "sequences, including included ones");
        assert_eq!(from_includes, 5_232, "sequences that came from a companion");
        assert_eq!(widest_animation, 4_050, "the longest animation in the game");
    }
}

#[cfg(test)]
mod anim_depot_tests {
    use super::*;

    /// The `prop_floor_button` model, read out of the real game: its bones,
    /// its sequences, and **how far its plate actually travels**.
    ///
    /// This is the test that says the animation decoder works, and nothing
    /// synthetic can replace it: the RLE stream, the `Quaternion64` constants,
    /// the `posscale` fixed point and the `poseToBone` inverse bind are all
    /// real data written by `studiomdl`, and every one of them is a silent
    /// wrong answer rather than an error.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_floor_button_model -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_floor_button_model_animates() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let model = StudioModel::load(&vfs, "models/props/portal_button.mdl").expect("the model");
        println!("{}: {} vertices", model.path, model.vertices.len());
        for (i, bone) in model.bones.iter().enumerate() {
            println!(
                "  bone {i} {:?} parent={:?} pos={:?}",
                bone.name, bone.parent, bone.pos
            );
        }
        for (i, seq) in model.sequences.iter().enumerate() {
            let anim = &model.animations[seq.anim];
            println!(
                "  seq {i} {:?} -> {:?} {} frames @ {} fps ({:.3}s), {} tracks",
                seq.label,
                anim.name,
                anim.frame_count,
                anim.fps,
                anim.duration(),
                anim.tracks.len()
            );
        }

        // Three bones, in a chain, and the plate sits 13.64 units up the model.
        assert_eq!(model.bones.len(), 3);
        assert_eq!(model.bones[0].name, "portal_button_export");
        assert_eq!(model.bones[1].parent, Some(0));
        assert_eq!(model.bones[2].parent, Some(1));
        assert!((model.bones[2].pos.y - 13.64).abs() < 0.01);

        // The two sequences `CPropFloorButton::LookUpAnimationSequences` asks
        // for by name, and a lookup that is case insensitive as Valve's is.
        let up = model.sequence("up").expect("an `up` sequence");
        let down = model.sequence("DOWN").expect("`down`, case insensitively");
        assert_eq!(model.animations[model.sequences[down].anim].frame_count, 11);
        assert_eq!(model.animations[model.sequences[down].anim].fps, 24.0);

        // …and the whole point: the plate is somewhere else at the end of
        // `down` than at the start of it.
        let travel = |sequence: usize| {
            let anim = &model.animations[model.sequences[sequence].anim];
            let start = anim::pose(&model.bones, Some(anim), 0.0);
            let end = anim::pose(&model.bones, Some(anim), 1.0);
            // Bone 2 is the plate; compare where it puts a bind-pose point.
            let probe = glam::Vec3::ZERO;
            (end[2].transform_point3(probe) - start[2].transform_point3(probe)).length()
        };
        let down_travel = travel(down);
        let up_travel = travel(up);
        println!("  plate travel: down {down_travel:.3} units, up {up_travel:.3} units");
        assert!(
            down_travel > 1.0,
            "the plate does not move over `down`: {down_travel}"
        );
        assert!(
            (down_travel - up_travel).abs() < 0.01,
            "`up` should retrace `down`: {up_travel} vs {down_travel}"
        );

        // Every vertex is moved by exactly one bone, and both bones are used —
        // the measurement the per-bone draw split depends on.
        let rigid = model.rigid_bones().expect("every vertex binds to one bone");
        let mut counts = [0usize; 3];
        for &bone in rigid {
            counts[bone as usize] += 1;
        }
        println!("  vertices per bone: {counts:?}");
        assert_eq!(counts[1] + counts[2], model.vertices.len());
        assert!(counts[2] > 0, "no vertex is on the moving plate");
    }

    /// **The test chamber door, read out of the real game** — the model
    /// `prop_testchamber_door` hard-codes, and the second model in the port
    /// that an entity animates.
    ///
    /// What it pins is the set of facts the class is written against, each of
    /// which is a silently wrong picture rather than an error if it moves:
    /// that `open` and `close` are *different lengths* so playing `open`
    /// backwards is a deliberate choice and not a shortcut; that `open` does
    /// not loop, so its cycle clamps; that `fadeouttime` is the 0.2 that
    /// `GetLastVisibleCycle` subtracts, which is what decides when
    /// `OnFullyOpen` fires; and that **every vertex answers to exactly one
    /// bone**, which is the precondition the per-bone draw split needs and
    /// which 74 of the models the game's props name fail.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_testchamber_door_model -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_testchamber_door_model_animates() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let model =
            StudioModel::load(&vfs, "models/props/portal_door_combined.mdl").expect("the model");
        println!("{}: {} vertices", model.path, model.vertices.len());
        for (i, bone) in model.bones.iter().enumerate() {
            println!("  bone {i} {:?} parent={:?}", bone.name, bone.parent);
        }
        for (i, seq) in model.sequences.iter().enumerate() {
            let anim = &model.animations[seq.anim];
            println!(
                "  seq {i} {:?} -> {:?} {} frames @ {} fps ({:.4}s), fadeout {}, {} tracks",
                seq.label,
                anim.name,
                anim.frame_count,
                anim.fps,
                anim.duration(),
                seq.fade_out_time,
                anim.tracks.len()
            );
        }

        // Two leaves on a spinner each, hung off a root — and two export bones
        // that nothing is skinned to.
        assert_eq!(model.bones.len(), 8);
        assert_eq!(model.bones[2].name, "portal_door_root");
        assert_eq!(model.bones[4].name, "portal_door_left");
        assert_eq!(model.bones[5].name, "portal_door_right");

        let open = model.sequence("open").expect("an `open` sequence");
        let close = model
            .sequence("CLOSE")
            .expect("`close`, case insensitively");
        let anim = |s: usize| &model.animations[model.sequences[s].anim];

        // **`open` is 23 frames and `close` is 36.** They are not the same
        // travel, so `CPropTestChamberDoor` shutting the door by playing
        // `open` at rate -1 is a decision rather than an accident — and it is
        // why this port must not "fix" it by using `close`.
        assert_eq!(anim(open).frame_count, 23);
        assert_eq!(anim(close).frame_count, 36);
        assert_eq!(anim(open).fps, 24.0);
        assert!((anim(open).duration() - 22.0 / 24.0).abs() < 1e-5);

        // Non-looping, so the cycle clamps at both ends and the door holds
        // whichever one it reached.
        assert_eq!(
            model.sequences[open].flags & anim::STUDIO_LOOPING,
            0,
            "`open` must not loop"
        );
        // `studiomdl`'s default, and what `GetLastVisibleCycle` subtracts:
        // 0.2 of 0.9167 seconds, so `OnFullyOpen` fires at cycle 0.782.
        assert_eq!(model.sequences[open].fade_out_time, 0.2);

        // The two leaves move, and `open` played backwards retraces itself —
        // which is the whole of how this door shuts.
        let travel = |sequence: usize, bone: usize| {
            let anim = &model.animations[model.sequences[sequence].anim];
            let start = anim::pose(&model.bones, Some(anim), 0.0);
            let end = anim::pose(&model.bones, Some(anim), 1.0);
            (end[bone].transform_point3(glam::Vec3::ZERO)
                - start[bone].transform_point3(glam::Vec3::ZERO))
            .length()
        };
        let (left, right) = (travel(open, 4), travel(open, 5));
        println!("  leaf travel over `open`: left {left:.3}, right {right:.3}");
        assert!(left > 1.0, "the left leaf does not move: {left}");
        assert!(right > 1.0, "the right leaf does not move: {right}");

        // Every vertex on exactly one bone — the precondition
        // `EntityModels`' per-bone draw split needs.
        let rigid = model.rigid_bones().expect("every vertex binds to one bone");
        let mut counts = vec![0usize; model.bones.len()];
        for &bone in rigid {
            counts[bone as usize] += 1;
        }
        println!("  vertices per bone: {counts:?}");
        assert_eq!(counts.iter().sum::<usize>(), model.vertices.len());
        assert!(
            counts[4] > 0 && counts[5] > 0,
            "the leaves have no geometry"
        );
        // …and one material, so the whole door is five draws and not fifty.
        assert_eq!(model.batches.len(), 1);
        assert_eq!(model.batches[0].bones.len(), 5);
    }

    /// `$includemodel`, read out of the real game: the panel arm whose 1,350
    /// animations live in a file the map never names.
    ///
    /// `models/anim_wp/room_transform/arm64x64_interior` and its `_rusty`
    /// twin are **898 of the 926 `prop_dynamic`s in Portal 2 that wear an
    /// include host**, and the only two of the nine whose vertices each answer
    /// to one bone — so they are the only ones this port can visibly animate,
    /// and they are what this tests.
    ///
    /// The check with teeth is the last one. The host's skeleton and the
    /// include's are the same sixteen bones **in a different order**, so
    /// posing the host with a merged animation and posing the *include* with
    /// its own unmerged one must agree bone-for-bone **by name**. That fails
    /// if the remap is skipped, if it is applied in the wrong direction, or if
    /// the two bind poses were not really interchangeable.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release an_included_model -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn an_included_model_supplies_the_sequences_a_map_asks_for() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        const HOST: &str = "models/anim_wp/room_transform/arm64x64_interior.mdl";
        const ANIM: &str = "models/anim_wp/room_transform/arm64x64_interior_animation.mdl";

        let clock = std::time::Instant::now();
        let model = StudioModel::load(&vfs, HOST).expect("the panel arm");
        let cost = clock.elapsed();
        // **What the merge costs, and where.** `anim.rs` expands every RLE
        // stream at load rather than walking it at draw, and this is the file
        // that puts a price on that: 1,350 animations over 55,007 frames and
        // sixteen bones, about 7 MB expanded. Read as a ratio — the same model
        // without its companion loads in about 3.5 ms — and note that it is a
        // *level load* cost and not a frame one: `engine::world::bench` does
        // not move.
        println!("  loaded in {cost:?}");
        println!(
            "{}: {} bones, {} sequences, {} animations, includes {:?}",
            model.path,
            model.bones.len(),
            model.sequences.len(),
            model.animations.len(),
            model.includes
        );

        assert_eq!(model.includes, vec![ANIM.to_owned()]);
        assert_eq!(model.bones.len(), 16);
        // One local sequence, `BindPose`, and 1,350 from the companion. The
        // host wins every label collision and there are none.
        assert_eq!(model.sequences.len(), 1 + 1_350);
        assert_eq!(model.sequences[0].label, "BindPose");

        // What a shipped map actually writes into `DefaultAnim`. Neither
        // resolves without the merge: the first is the commonest of the 270
        // labels in the game that only an include has, and the second is the
        // host's own.
        let asked = model
            .sequence("makeramp_02open_idleend")
            .expect("a label only the include has");
        assert!(model.sequence("bindpose").is_some(), "and the host's own");

        // The animation arrived with data rather than as an empty husk: this
        // file keeps all 1,350 of its animations inline, where the other seven
        // include models in the game keep most of theirs in an `.ani` this
        // port does not read.
        let animation = model.animation(asked).expect("an animation");
        println!(
            "  {:?}: {} frames @ {} fps ({:.3}s), {} tracks",
            animation.name,
            animation.frame_count,
            animation.fps,
            animation.duration(),
            animation.tracks.len()
        );
        assert!(!animation.tracks.is_empty());
        // Every track names a bone of the **host**, which is what the remap is
        // for.
        assert!(animation.tracks.iter().all(|t| t.bone < model.bones.len()));

        // **Most of what this buys is a still pose, not motion.** The panel
        // arms' `DefaultAnim` keys are overwhelmingly `…_idle` and
        // `…_idleend`, and an `_idleend` is *one frame*: the shape the arm
        // holds once it has finished unfolding. So the thing to assert is that
        // the pose is not the bind pose — which is exactly what those 898
        // props were stuck in before the merge.
        let posed = anim::pose(&model.bones, Some(animation), 0.0);
        let bind = anim::pose(&model.bones, None, 0.0);
        let bend = posed
            .iter()
            .zip(&bind)
            .map(|(a, b)| {
                (a.transform_point3(glam::Vec3::ZERO) - b.transform_point3(glam::Vec3::ZERO))
                    .length()
            })
            .fold(0.0f32, f32::max);
        println!("  furthest bone sits {bend:.3} units off the bind pose");
        assert!(bend > 1.0, "`{}` is the bind pose", animation.name);

        // And the ones that do move, move. The longest animation in the file
        // is the honest place to look for that, since which sequences a map
        // plays through is `server/`'s question and not this module's.
        let (longest, animation) = model
            .animations
            .iter()
            .enumerate()
            .max_by_key(|(_, a)| a.frame_count)
            .expect("an animation");
        let travel = |cycle_a: f32, cycle_b: f32| {
            let a = anim::pose(&model.bones, Some(animation), cycle_a);
            let b = anim::pose(&model.bones, Some(animation), cycle_b);
            a.iter()
                .zip(&b)
                .map(|(a, b)| {
                    (b.transform_point3(glam::Vec3::ZERO) - a.transform_point3(glam::Vec3::ZERO))
                        .length()
                })
                .fold(0.0f32, f32::max)
        };
        println!(
            "  longest animation {longest} {:?}: {} frames, furthest bone travels {:.3} units",
            animation.name,
            animation.frame_count,
            travel(0.0, 1.0)
        );
        assert!(animation.frame_count > 1);
        assert!(
            travel(0.0, 1.0) > 1.0,
            "nothing moved over the longest animation"
        );

        // ------------------------------------------------------------------
        // The remap, checked against the animation in its own frame.
        // ------------------------------------------------------------------
        let bytes = vfs.read(ANIM).expect("the companion");
        let companion = Mdl::parse(ANIM.to_owned(), &bytes).expect("parse the companion");

        // The two skeletons are the same names in a different order — which is
        // what makes the remap load-bearing rather than decorative. Pin it, so
        // that a future file that happened to agree could not quietly turn
        // this test into a tautology.
        let host_names: Vec<&str> = model.bones.iter().map(|b| b.name.as_str()).collect();
        let their_names: Vec<&str> = companion.bones.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(host_names.len(), their_names.len());
        assert_ne!(host_names, their_names, "the orders should differ");

        let theirs = companion
            .animations
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&animation.name))
            .expect("the companion's own copy of it");
        for cycle in [0.0, 0.37, 1.0] {
            let ours = anim::pose(&model.bones, Some(animation), cycle);
            let theirs = anim::pose(&companion.bones, Some(theirs), cycle);
            for (i, bone) in model.bones.iter().enumerate() {
                let j = companion
                    .bones
                    .iter()
                    .position(|b| b.name.eq_ignore_ascii_case(&bone.name))
                    .expect("every bone is in both");
                let (a, b) = (ours[i], theirs[j]);
                let worst = a
                    .to_cols_array()
                    .iter()
                    .zip(b.to_cols_array())
                    .map(|(x, y)| (x - y).abs())
                    .fold(0.0f32, f32::max);
                assert!(
                    worst < 1e-4,
                    "bone {:?} at cycle {cycle}: {a:?} vs {b:?}",
                    bone.name
                );
            }
        }
        println!("  all 16 bones agree with the companion's own frame at three cycles");

        // The other eight hosts in the game, for the record — and the
        // measurement that says what is still missing from each.
        for path in [
            "models/anim_wp/room_transform/arm64x64_interior_rusty.mdl",
            "models/npcs/personality_sphere/personality_sphere_skins.mdl",
            "models/player/eggbot/eggbot.mdl",
            "models/player/ballbot/ballbot.mdl",
            "models/player/br/headless_s8player.mdl",
            "models/player/chell/player.mdl",
            "models/player/chell/headless_player.mdl",
            "models/npcs/glados/glados_wheatley_boss.mdl",
        ] {
            let m = StudioModel::load(&vfs, path).expect("a host model");
            let with_data = m.animations.iter().filter(|a| !a.tracks.is_empty()).count();
            println!(
                "  {path}: {} sequences, {}/{} animations with data, rigid={}",
                m.sequences.len(),
                with_data,
                m.animations.len(),
                m.rigid_bones().is_some()
            );
            assert_eq!(m.includes.len(), 1, "{path} should have merged one include");
            assert!(!m.sequences.is_empty(), "{path} has no sequences");
        }
    }
}
