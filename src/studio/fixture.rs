//! Synthetic `.mdl` / `.vvd` / `.vtx` trios for the tests.
//!
//! Built byte by byte rather than checked in as files, for the reason
//! `world::bsp`'s fixtures are: a test that can *say* what it is testing —
//! "three meshes, the second one offset by four vertices" — is worth more than
//! an opaque binary, and the layout arithmetic is exactly what is under test.
//!
//! The shapes here mirror what was measured in the shipped game
//! (`portdocs/STUDIO.md` §3): one model per body part, one strip group per
//! mesh, one trilist strip per strip group.

use super::mdl::{MESH_STRIDE, MODEL_STRIDE};

/// The `.mdl` header size this fixture writes — the real one is larger, but
/// every field after `bodypartindex` is zero here and unread.
const HEADER: usize = 256;

/// One mesh: a material, a slice of its model's vertices, and triangles naming
/// vertices *within that slice*.
pub(crate) struct MeshSpec {
    pub material: usize,
    pub vertex_offset: u32,
    pub vertex_count: u32,
    /// `origMeshVertID` triples — indices relative to the mesh.
    pub triangles: Vec<[u16; 3]>,
}

/// One model, as a list of meshes over a contiguous vertex range.
pub(crate) struct ModelSpec {
    pub vertex_index: u32,
    pub vertex_count: u32,
    pub meshes: Vec<MeshSpec>,
}

/// A whole model trio.
pub(crate) struct Spec {
    pub checksum: u32,
    /// Bare texture names, what a mesh's `material` indexes.
    pub textures: Vec<String>,
    /// `cdtextures` search directories, without trailing slashes.
    pub texture_dirs: Vec<String>,
    /// One entry per body part; every body part holds exactly one model.
    pub body_parts: Vec<ModelSpec>,
    /// How many vertices the `.vvd` pool holds, before any fixup culling.
    pub pool_vertices: usize,
    /// `numLODVertexes[0..]`. Defaults to `pool_vertices` at every LOD.
    pub lod_vertex_counts: Option<[i32; 8]>,
    /// `(lod, sourceVertexID, numVertexes)` runs. Empty means no table.
    pub fixups: Vec<(i32, i32, i32)>,
    /// Whether the `.vvd` carries a tangent block.
    pub tangents: bool,
    /// `StripGroupHeader_t::flags`. `STRIPGROUP_IS_HWSKINNED` by default,
    /// which is what every shipped static prop sets.
    pub strip_group_flags: u8,
    /// `StripHeader_t::flags`. `STRIP_IS_TRILIST` by default.
    pub strip_flags: u8,
    /// Overrides for the three file version fields, so a test can present a
    /// file this engine refuses.
    pub mdl_version: i32,
    pub vvd_version: i32,
    pub vtx_version: i32,
    /// Checksums written into the companions, when they should disagree with
    /// the `.mdl`'s.
    pub vvd_checksum: Option<u32>,
    pub vtx_checksum: Option<u32>,
}

impl Default for Spec {
    fn default() -> Spec {
        Spec {
            checksum: 0x1234_5678,
            textures: vec!["wall".to_owned()],
            texture_dirs: vec!["models/test".to_owned()],
            body_parts: vec![ModelSpec {
                vertex_index: 0,
                vertex_count: 3,
                meshes: vec![MeshSpec {
                    material: 0,
                    vertex_offset: 0,
                    vertex_count: 3,
                    triangles: vec![[0, 1, 2]],
                }],
            }],
            pool_vertices: 3,
            lod_vertex_counts: None,
            fixups: Vec::new(),
            tangents: true,
            strip_group_flags: 0x02,
            strip_flags: 0x01,
            mdl_version: 49,
            vvd_version: 4,
            vtx_version: 7,
            vvd_checksum: None,
            vtx_checksum: None,
        }
    }
}

/// The position the fixture gives pool vertex `i`, so a test can assert that a
/// particular vertex reached a particular index.
pub(crate) fn pool_position(i: usize) -> [f32; 3] {
    [i as f32, i as f32 * 2.0, i as f32 * 3.0]
}

impl Spec {
    /// Builds the three files.
    pub(crate) fn build(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (self.mdl(), self.vvd(), self.vtx())
    }

    fn mdl(&self) -> Vec<u8> {
        let mut out = vec![0u8; HEADER];
        out[0..4].copy_from_slice(b"IDST");
        put_i32(&mut out, 4, self.mdl_version);
        put_i32(&mut out, 8, self.checksum as i32);
        // name[64] at 12
        let name = b"test.mdl";
        out[12..12 + name.len()].copy_from_slice(name);
        // illumposition at 92, view_bbmin/max at 128/140 — left zero.

        // Texture name strings, then the texture structs that point back at them.
        let mut name_offsets = Vec::new();
        for texture in &self.textures {
            name_offsets.push(out.len());
            out.extend_from_slice(texture.as_bytes());
            out.push(0);
        }
        align4(&mut out);
        let texture_base = out.len();
        out.resize(texture_base + self.textures.len() * 64, 0);
        for (i, name_at) in name_offsets.iter().enumerate() {
            let at = texture_base + i * 64;
            // sznameindex is relative to the texture struct.
            put_i32(&mut out, at, *name_at as i32 - at as i32);
        }
        put_i32(&mut out, 204, self.textures.len() as i32);
        put_i32(&mut out, 208, texture_base as i32);

        // cdtextures: strings, then a table of file-absolute offsets to them.
        let mut dir_offsets = Vec::new();
        for dir in &self.texture_dirs {
            dir_offsets.push(out.len());
            out.extend_from_slice(dir.as_bytes());
            out.push(0);
        }
        align4(&mut out);
        let cd_base = out.len();
        for offset in &dir_offsets {
            let at = out.len();
            out.resize(at + 4, 0);
            put_i32(&mut out, at, *offset as i32);
        }
        put_i32(&mut out, 212, self.texture_dirs.len() as i32);
        put_i32(&mut out, 216, cd_base as i32);

        // Body parts, then each one's model, then each model's meshes. Written
        // in three passes because a body part's `modelindex` is relative to
        // itself and so cannot be known until the models are placed.
        let body_part_base = out.len();
        out.resize(body_part_base + self.body_parts.len() * 16, 0);

        for (i, spec) in self.body_parts.iter().enumerate() {
            let part_at = body_part_base + i * 16;
            let model_at = out.len();
            out.resize(model_at + MODEL_STRIDE, 0);

            put_i32(&mut out, part_at + 4, 1); // nummodels
            put_i32(&mut out, part_at + 12, model_at as i32 - part_at as i32);

            let mesh_base = out.len();
            out.resize(mesh_base + spec.meshes.len() * MESH_STRIDE, 0);

            put_i32(&mut out, model_at + 72, spec.meshes.len() as i32);
            put_i32(&mut out, model_at + 76, mesh_base as i32 - model_at as i32);
            put_i32(&mut out, model_at + 80, spec.vertex_count as i32);
            // vertexindex and tangentsindex are *byte* offsets.
            put_i32(&mut out, model_at + 84, (spec.vertex_index * 48) as i32);
            put_i32(&mut out, model_at + 88, (spec.vertex_index * 16) as i32);

            for (j, mesh) in spec.meshes.iter().enumerate() {
                let at = mesh_base + j * MESH_STRIDE;
                put_i32(&mut out, at, mesh.material as i32);
                put_i32(&mut out, at + 4, model_at as i32 - at as i32); // modelindex
                put_i32(&mut out, at + 8, mesh.vertex_count as i32);
                put_i32(&mut out, at + 12, mesh.vertex_offset as i32);
            }
        }
        put_i32(&mut out, 232, self.body_parts.len() as i32);
        put_i32(&mut out, 236, body_part_base as i32);

        out
    }

    fn vvd(&self) -> Vec<u8> {
        let mut out = vec![0u8; 64];
        out[0..4].copy_from_slice(b"IDSV");
        put_i32(&mut out, 4, self.vvd_version);
        put_i32(&mut out, 8, self.vvd_checksum.unwrap_or(self.checksum) as i32);
        put_i32(&mut out, 12, 1); // numLODs

        let counts = self
            .lod_vertex_counts
            .unwrap_or([self.pool_vertices as i32; 8]);
        for (i, count) in counts.iter().enumerate() {
            put_i32(&mut out, 16 + i * 4, *count);
        }

        put_i32(&mut out, 48, self.fixups.len() as i32);
        let fixup_start = if self.fixups.is_empty() {
            0
        } else {
            let base = out.len();
            for (lod, source, count) in &self.fixups {
                let at = out.len();
                out.resize(at + 12, 0);
                put_i32(&mut out, at, *lod);
                put_i32(&mut out, at + 4, *source);
                put_i32(&mut out, at + 8, *count);
            }
            base
        };
        put_i32(&mut out, 52, fixup_start as i32);

        let vertex_start = out.len();
        put_i32(&mut out, 56, vertex_start as i32);
        for i in 0..self.pool_vertices {
            let at = out.len();
            out.resize(at + 48, 0);
            // 0..16 is the bone weight block: one bone, full weight.
            put_f32(&mut out, at, 1.0);
            out[at + 15] = 1; // numbones
            let position = pool_position(i);
            for (k, value) in position.iter().enumerate() {
                put_f32(&mut out, at + 16 + k * 4, *value);
            }
            put_f32(&mut out, at + 28, 0.0); // normal x
            put_f32(&mut out, at + 32, 0.0);
            put_f32(&mut out, at + 36, 1.0); // normal z
            put_f32(&mut out, at + 40, i as f32); // texcoord u
            put_f32(&mut out, at + 44, 0.5);
        }

        if self.tangents {
            let tangent_start = out.len();
            put_i32(&mut out, 60, tangent_start as i32);
            for i in 0..self.pool_vertices {
                let at = out.len();
                out.resize(at + 16, 0);
                put_f32(&mut out, at, 1.0);
                put_f32(&mut out, at + 4, 0.0);
                put_f32(&mut out, at + 8, 0.0);
                // The binormal sign carries the pool index so a permutation is
                // visible in the tangents as well as the positions.
                put_f32(&mut out, at + 12, i as f32);
            }
        }

        out
    }

    fn vtx(&self) -> Vec<u8> {
        let mut out = vec![0u8; 36];
        put_i32(&mut out, 0, self.vtx_version);
        put_i32(&mut out, 16, self.vtx_checksum.unwrap_or(self.checksum) as i32);
        put_i32(&mut out, 20, 1); // numLODs

        let body_part_base = out.len();
        out.resize(body_part_base + self.body_parts.len() * 8, 0);
        put_i32(&mut out, 28, self.body_parts.len() as i32);
        put_i32(&mut out, 32, body_part_base as i32);

        for (i, spec) in self.body_parts.iter().enumerate() {
            let part_at = body_part_base + i * 8;
            let model_at = out.len();
            out.resize(model_at + 8, 0);
            put_i32(&mut out, part_at, 1); // numModels
            put_i32(&mut out, part_at + 4, model_at as i32 - part_at as i32);

            let lod_at = out.len();
            out.resize(lod_at + 12, 0);
            put_i32(&mut out, model_at, 1); // numLODs
            put_i32(&mut out, model_at + 4, lod_at as i32 - model_at as i32);

            let mesh_base = out.len();
            out.resize(mesh_base + spec.meshes.len() * 9, 0);
            put_i32(&mut out, lod_at, spec.meshes.len() as i32);
            put_i32(&mut out, lod_at + 4, mesh_base as i32 - lod_at as i32);

            for (j, mesh) in spec.meshes.iter().enumerate() {
                let mesh_at = mesh_base + j * 9;
                let group_at = out.len();
                out.resize(group_at + 33, 0);
                put_i32(&mut out, mesh_at, 1); // numStripGroups
                put_i32(&mut out, mesh_at + 4, group_at as i32 - mesh_at as i32);

                // The group's vertex table: one entry per distinct index used,
                // written as the identity so `origMeshVertID` reads directly.
                let group_vertex_base = out.len();
                for v in 0..mesh.vertex_count {
                    let at = out.len();
                    out.resize(at + 9, 0);
                    out[at + 3] = 1; // numBones
                    put_u16(&mut out, at + super::vtx::ORIG_MESH_VERT_ID, v as u16);
                }
                let index_base = out.len();
                for triangle in &mesh.triangles {
                    for index in triangle {
                        let at = out.len();
                        out.resize(at + 2, 0);
                        put_u16(&mut out, at, *index);
                    }
                }
                let index_count = mesh.triangles.len() * 3;
                let strip_at = out.len();
                out.resize(strip_at + 35, 0);

                put_i32(&mut out, group_at, mesh.vertex_count as i32);
                put_i32(&mut out, group_at + 4, group_vertex_base as i32 - group_at as i32);
                put_i32(&mut out, group_at + 8, index_count as i32);
                put_i32(&mut out, group_at + 12, index_base as i32 - group_at as i32);
                put_i32(&mut out, group_at + 16, 1); // numStrips
                put_i32(&mut out, group_at + 20, strip_at as i32 - group_at as i32);
                out[group_at + 24] = self.strip_group_flags;

                put_i32(&mut out, strip_at, index_count as i32);
                put_i32(&mut out, strip_at + 4, 0); // indexOffset
                put_i32(&mut out, strip_at + 8, mesh.vertex_count as i32);
                put_i32(&mut out, strip_at + 12, 0); // vertOffset
                out[strip_at + 18] = self.strip_flags;
            }
        }

        out
    }
}

fn put_i32(out: &mut [u8], at: usize, value: i32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_f32(out: &mut [u8], at: usize, value: f32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn align4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
