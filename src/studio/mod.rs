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

mod build;
#[cfg(test)]
mod fixture;
mod mdl;
mod vtx;
mod vvd;

pub use mdl::{Mdl, StudioFlags};
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
    /// `view_bbmin` / `view_bbmax` — the render bounds, in model space.
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

        let mdl = Mdl::parse(mdl_path.clone(), &read(&mdl_path)?)?;
        let vvd_bytes = read(&vvd_path)?;
        let vvd = Vvd::parse(vvd_path, &vvd_bytes)?;
        let vtx_bytes = read(&vtx_path)?;
        let vtx = Vtx::parse(vtx_path, &vtx_bytes)?;

        build::build(&mdl, &vvd, &vtx, |candidates| {
            candidates
                .iter()
                .find(|candidate| vfs.exists(&format!("materials/{candidate}.vmt")))
                .cloned()
        })
    }

    /// Total triangles across every batch.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
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
        let body_part_base =
            i32::from_le_bytes(mdl_bytes[236..240].try_into().unwrap()) as usize;
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

    /// The baked static light is black until a `.vhv` supplies it — white would
    /// be twice the brightest value `vrad` can bake, since the stream is
    /// pre-multiplied by a half.
    #[test]
    fn vertices_start_with_no_baked_light() {
        let model = assemble_spec(&Spec::default()).expect("a well-formed trio");
        assert!(model.vertices.iter().all(|v| v.color == [0.0; 4]));
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
            let is_static = Mdl::parse(path.clone(), &vfs.read(path).expect("read the .mdl"))
                .map(|mdl| mdl.flags.contains(StudioFlags::STATIC_PROP))
                .unwrap_or(false);

            match StudioModel::load(&vfs, path) {
                Ok(model) => {
                    loaded += 1;
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
        for line in animated_failed.iter().take(5) {
            println!("  (not a static prop) {line}");
        }
        for line in failed.iter().take(20) {
            println!("  {line}");
        }
        assert!(failed.is_empty(), "{} static props failed to load", failed.len());
    }
}
