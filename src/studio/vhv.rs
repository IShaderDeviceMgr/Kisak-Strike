//! Reading `.vhv` — the per-placement baked vertex lighting.
//!
//! `HardwareVerts::FileHeader_t` (`public/materialsystem/hardwareverts.h:47`),
//! loaded by `CStaticProp`'s colour-mesh path (`engine/l_studio.cpp:4290`).
//!
//! `vrad` bakes one colour per vertex **per placed prop** and writes it into
//! the map's pak lump as `sp_hdr_<index>.vhv` (or `sp_<index>.vhv` for an LDR
//! compile), where `<index>` is the prop's position in the `sprp` lump. Portal
//! 2 ships **56,955 of them**, one for every static prop in the game — which is
//! why this is a second vertex stream rather than a field of
//! [`ModelVertex`](crate::materials::mesh::ModelVertex): the geometry is shared
//! by every placement and the lighting is not.
//!
//! # Layout
//!
//! Everything here is `#pragma pack(1)`.
//!
//! ```text
//! FileHeader_t                                       40 bytes
//!   0  version        VHV_VERSION, 2
//!   4  checksum       must match the .mdl's
//!   8  vertexFlags
//!  12  vertexSize     bytes per vertex — 4 on every shipped file
//!  16  vertexes       total across every mesh
//!  20  meshes
//!  24  unused[4]
//! MeshHeader_t[meshes]                               28 bytes each
//!   0  lod            which LOD this mesh belongs to
//!   4  vertexes
//!   8  offset         **from the start of the file**, sector-aligned
//!  12  unused[4]
//! ```
//!
//! The header's own comment explains the alignment: "the streamable component
//! starts and ends on a sector (512) aligned boundary", so the first mesh's
//! data begins at 512 and not at 40 plus the mesh table. Computing the offset
//! rather than reading it puts every colour on the wrong vertex.
//!
//! # What the shipped files contain
//!
//! Measured over `sp_a1_intro1`'s 1,080: **every one is version 2 with
//! `vertexSize` 4** — RGBA8, one lighting stream, which is
//! `r_staticlight_streams` 1. Most (963) have a single mesh at LOD 0; the rest
//! carry the same meshes again per LOD, up to 12 entries for a model with two
//! meshes and six LODs. So the meshes for one LOD are a *subset*, selected by
//! `m_nLod` — see [`Vhv::colors`].

use super::mdl::Reader;
use super::StudioError;
use crate::materials::mesh::StaticLightVertex;

/// `VHV_VERSION` (`hardwareverts.h:23`).
const VERSION: i32 = 2;

/// `sizeof(HardwareVerts::FileHeader_t)`.
const HEADER_SIZE: usize = 40;

/// `sizeof(HardwareVerts::MeshHeader_t)`.
const MESH_STRIDE: usize = 28;

/// The only vertex size this reads: RGBA8.
///
/// `r_staticlight_streams` picks 1 or 3 streams in the original, and 3 is the
/// cascaded-shadow path's — a console-era feature this port does not have.
/// Every shipped file is 4, so a wider one is refused rather than misread.
const VERTEX_SIZE: u32 = 4;

/// One mesh's block of colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VhvMesh {
    pub lod: u32,
    pub vertex_count: u32,
    /// From the start of the file.
    pub offset: u32,
}

/// A parsed `.vhv`.
#[derive(Debug, Clone)]
pub struct Vhv {
    pub path: String,
    /// Must match the `.mdl`'s, or the colours belong to different geometry.
    pub checksum: u32,
    /// The header's total, across every LOD.
    pub vertex_count: u32,
    pub meshes: Vec<VhvMesh>,
}

impl Vhv {
    pub fn parse(path: String, bytes: &[u8]) -> Result<Vhv, StudioError> {
        let r = Reader {
            path: &path,
            what: "a .vhv",
            bytes,
        };

        if bytes.len() < HEADER_SIZE {
            return Err(StudioError::TooShort {
                path,
                what: "a hardware-verts header",
                size: bytes.len(),
                needed: HEADER_SIZE,
            });
        }

        let version = r.i32(0)?;
        if version != VERSION {
            return Err(StudioError::Version {
                path,
                what: ".vhv",
                version,
                expected: VERSION,
            });
        }

        let checksum = r.u32(4)?;
        let vertex_size = r.u32(12)?;
        if vertex_size != VERTEX_SIZE {
            return Err(r.corrupt(format!(
                "vertexSize is {vertex_size}; this engine reads {VERTEX_SIZE}-byte \
                 (RGBA8) lighting only"
            )));
        }
        let vertex_count = r.u32(16)?;
        let mesh_count = r.count(20, "meshes")?;

        let mut meshes = Vec::with_capacity(mesh_count);
        for i in 0..mesh_count {
            let at = HEADER_SIZE + i * MESH_STRIDE;
            let mesh = VhvMesh {
                lod: r.u32(at)?,
                vertex_count: r.u32(at + 4)?,
                offset: r.u32(at + 8)?,
            };
            let end = mesh.offset as u64 + u64::from(mesh.vertex_count) * u64::from(vertex_size);
            if end > bytes.len() as u64 {
                return Err(r.corrupt(format!(
                    "mesh {i}'s {} vertices at {} run past the file's {} bytes",
                    mesh.vertex_count,
                    mesh.offset,
                    bytes.len()
                )));
            }
            meshes.push(mesh);
        }

        Ok(Vhv {
            path,
            checksum,
            vertex_count,
            meshes,
        })
    }

    /// The meshes belonging to one LOD, in file order.
    pub fn lod_meshes(&self, lod: u32) -> impl Iterator<Item = &VhvMesh> {
        self.meshes.iter().filter(move |m| m.lod == lod)
    }

    /// The colours for one LOD, scattered onto the model's vertex pool.
    ///
    /// `meshes` are the model's studio meshes at that LOD, in the same order,
    /// carrying the hardware vertex order a `.vhv` block is written in — see
    /// [`HardwareMesh`](super::HardwareMesh), which is the whole subtlety here.
    /// Returns a buffer of `vertex_count` entries with every hardware vertex's
    /// colour at its pool index and [`StaticLightVertex::UNLIT`] everywhere
    /// else (a pool vertex no LOD-0 strip references has no baked colour and
    /// is never drawn at this LOD either).
    ///
    /// **Matched positionally, and checked.** The `n`th mesh of this LOD is the
    /// `n`th studio mesh, and the two must agree on how many vertices that is;
    /// `None` when they do not, which is what the original does too — it
    /// compares the same counts and abandons the file (`l_studio.cpp:4345`)
    /// rather than lighting a prop with another mesh's colours. It is worth
    /// checking rather than trusting: a `.vhv` is generated from the `.mdl` the
    /// map was compiled against, and a model updated afterwards keeps a
    /// plausible-looking file of exactly the wrong shape.
    pub fn colors(
        &self,
        bytes: &[u8],
        lod: u32,
        meshes: &[super::HardwareMesh],
        vertex_count: usize,
    ) -> Option<Vec<StaticLightVertex>> {
        let blocks: Vec<_> = self.lod_meshes(lod).collect();
        // **Empty meshes are not written.** `vrad` emits a block per mesh that
        // produces geometry, and a studio mesh with no strip groups at this LOD
        // produces none — `models/npcs/turret/turret_debris_lrg` has eight
        // meshes of which three are empty and five are in the file. Matching
        // the lists without dropping them shifts every later block onto the
        // wrong mesh.
        let meshes: Vec<_> = meshes.iter().filter(|m| !m.vertices.is_empty()).collect();
        if blocks.len() != meshes.len() {
            return None;
        }

        let mut out = vec![StaticLightVertex::UNLIT; vertex_count];
        for (block, mesh) in blocks.iter().zip(&meshes) {
            if block.vertex_count as usize != mesh.vertices.len() {
                return None;
            }
            let at = block.offset as usize;
            for (i, &vertex) in mesh.vertices.iter().enumerate() {
                let rgba = bytes.get(at + i * 4..at + i * 4 + 4)?;
                // A pool vertex named by two hardware vertices gets the last
                // one's colour. That happens where a strip group splits a
                // seam, and `vrad` bakes the same value on both sides.
                *out.get_mut(vertex as usize)? =
                    StaticLightVertex::new([rgba[0], rgba[1], rgba[2], rgba[3]]);
            }
        }
        Some(out)
    }
}

/// The name `vrad` gave the lighting for prop `index`.
///
/// `sp_hdr_%d.vhv` for an HDR compile and `sp_%d.vhv` for an LDR one
/// (`engine/l_studio.cpp:4207`). Portal 2 ships HDR-only maps, so the first is
/// what actually exists — but the name is derived from the map's lighting lump
/// rather than assumed, because a map compiled either way is legal and the
/// wrong name is simply a missing file, which reads as "this prop has no baked
/// lighting" and is indistinguishable from the real thing.
pub fn prop_lighting_path(index: usize, hdr: bool) -> String {
    match hdr {
        true => format!("sp_hdr_{index}.vhv"),
        false => format!("sp_{index}.vhv"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::studio::HardwareMesh;

    /// Builds a `.vhv` from `hardwareverts.h`'s field list rather than from the
    /// reader above — `portdocs/STUDIO.md` §11.1 is what that rule is for.
    ///
    /// `meshes` is `(lod, colours)`. Blocks are laid out from a 512-byte
    /// boundary, as `vrad` writes them.
    fn vhv(checksum: u32, meshes: &[(u32, Vec<[u8; 4]>)]) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_SIZE + meshes.len() * MESH_STRIDE];
        let total: u32 = meshes.iter().map(|(_, c)| c.len() as u32).sum();
        out[0..4].copy_from_slice(&(VERSION as u32).to_le_bytes());
        out[4..8].copy_from_slice(&checksum.to_le_bytes());
        out[8..12].copy_from_slice(&4u32.to_le_bytes()); // vertexFlags
        out[12..16].copy_from_slice(&VERTEX_SIZE.to_le_bytes());
        out[16..20].copy_from_slice(&total.to_le_bytes());
        out[20..24].copy_from_slice(&(meshes.len() as u32).to_le_bytes());

        // "The streamable component starts and ends on a sector (512) aligned
        // boundary" — hardwareverts.h's own comment, and the reason the offset
        // is a field rather than something to compute.
        out.resize(512, 0);
        for (i, (lod, colours)) in meshes.iter().enumerate() {
            let at = HEADER_SIZE + i * MESH_STRIDE;
            out[at..at + 4].copy_from_slice(&lod.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&(colours.len() as u32).to_le_bytes());
            let offset = out.len() as u32;
            out[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
            for colour in colours {
                out.extend_from_slice(colour);
            }
        }
        out
    }

    fn mesh(vertices: &[u32]) -> HardwareMesh {
        HardwareMesh {
            vertices: vertices.to_vec(),
        }
    }

    #[test]
    fn a_minimal_file_parses() {
        let bytes = vhv(0x1234, &[(0, vec![[1, 2, 3, 4], [5, 6, 7, 8]])]);
        let file = Vhv::parse("test.vhv".into(), &bytes).expect("well-formed");
        assert_eq!(file.checksum, 0x1234);
        assert_eq!(file.vertex_count, 2);
        assert_eq!(file.meshes.len(), 1);
        assert_eq!(file.meshes[0].offset, 512);
    }

    /// The colours land on the pool vertices the *hardware* order names, not on
    /// a run starting at the mesh's first vertex.
    ///
    /// This is `HardwareMesh`'s whole reason for existing: Valve's runtime
    /// compacts a model's vertices per LOD and a `.vhv` is written against that
    /// numbering, so the `n`th colour belongs to the `n`th entry of the `.vtx`
    /// strip-group vertex table. Reading it as a run lights 125 of
    /// `sp_a1_intro1`'s 1,080 props from the wrong vertices.
    #[test]
    fn colours_follow_the_hardware_vertex_order() {
        let red = [255, 0, 0, 255];
        let green = [0, 255, 0, 255];
        let bytes = vhv(0, &[(0, vec![red, green])]);
        let file = Vhv::parse("test.vhv".into(), &bytes).unwrap();

        // Hardware vertex 0 is pool vertex 3, hardware vertex 1 is pool 1.
        let colors = file
            .colors(&bytes, 0, &[mesh(&[3, 1])], 5)
            .expect("the shapes agree");
        assert_eq!(colors[3], StaticLightVertex::new(red));
        assert_eq!(colors[1], StaticLightVertex::new(green));
        // Everything the LOD does not reference stays unlit.
        for i in [0, 2, 4] {
            assert_eq!(colors[i], StaticLightVertex::UNLIT, "pool vertex {i}");
        }
    }

    /// Only the requested LOD's blocks are read, and they are matched in order.
    #[test]
    fn a_lower_lods_blocks_are_skipped() {
        let a = [1, 1, 1, 255];
        let b = [2, 2, 2, 255];
        let bytes = vhv(0, &[(0, vec![a]), (1, vec![b, b]), (2, vec![b])]);
        let file = Vhv::parse("test.vhv".into(), &bytes).unwrap();
        assert_eq!(file.lod_meshes(0).count(), 1);
        let colors = file.colors(&bytes, 0, &[mesh(&[0])], 1).expect("lod 0");
        assert_eq!(colors[0], StaticLightVertex::new(a));
    }

    /// `vrad` writes no block for a mesh with no geometry, so the model's empty
    /// meshes are dropped before the lists are matched. Keeping them shifts
    /// every later block onto the wrong mesh —
    /// `models/npcs/turret/turret_debris_lrg` has eight meshes and five blocks.
    #[test]
    fn empty_meshes_are_not_written_and_must_not_be_matched() {
        let a = [1, 1, 1, 255];
        let b = [2, 2, 2, 255];
        let bytes = vhv(0, &[(0, vec![a]), (0, vec![b])]);
        let file = Vhv::parse("test.vhv".into(), &bytes).unwrap();

        let model = [mesh(&[0]), mesh(&[]), mesh(&[1])];
        let colors = file.colors(&bytes, 0, &model, 2).expect("the empty one drops out");
        assert_eq!(colors[0], StaticLightVertex::new(a));
        assert_eq!(colors[1], StaticLightVertex::new(b));
    }

    /// A file whose blocks do not describe this model is refused rather than
    /// scattered — which is the check the original makes too
    /// (`l_studio.cpp:4345`), and the one that actually protects a prop from
    /// another model's colours.
    #[test]
    fn a_file_that_does_not_describe_the_model_is_refused() {
        let bytes = vhv(0, &[(0, vec![[1, 2, 3, 4], [5, 6, 7, 8]])]);
        let file = Vhv::parse("test.vhv".into(), &bytes).unwrap();
        assert!(file.colors(&bytes, 0, &[mesh(&[0, 1, 2])], 3).is_none(), "count");
        assert!(
            file.colors(&bytes, 0, &[mesh(&[0]), mesh(&[1])], 2).is_none(),
            "mesh count"
        );
    }

    #[test]
    fn a_version_or_vertex_size_this_reader_does_not_know_is_refused() {
        let mut bytes = vhv(0, &[(0, vec![[0; 4]])]);
        bytes[0..4].copy_from_slice(&3u32.to_le_bytes());
        assert!(Vhv::parse("test.vhv".into(), &bytes).is_err(), "version");

        let mut bytes = vhv(0, &[(0, vec![[0; 4]])]);
        // 12 bytes a vertex is `r_staticlight_streams` 3, the console path.
        bytes[12..16].copy_from_slice(&12u32.to_le_bytes());
        assert!(Vhv::parse("test.vhv".into(), &bytes).is_err(), "vertex size");
    }

    /// HDR and LDR compiles write different names for the same prop.
    #[test]
    fn the_name_follows_the_maps_lighting() {
        assert_eq!(prop_lighting_path(7, true), "sp_hdr_7.vhv");
        assert_eq!(prop_lighting_path(7, false), "sp_7.vhv");
    }
}
