//! Reading the `.bsp` file.
//!
//! Replaces the lump-reading half of `engine/modelloader.cpp` (7,587 lines) and
//! `utils/common/bsplib.cpp`'s `LoadBSPFile`. What is here is only the part of
//! the format the renderer walks; `portdocs/ENGINE.md` §7.14 has the rest of
//! the subsystem, and [`super`] lists what is deliberately not read yet.
//!
//! **The format is fixed** (`PORTING.md`, "Format is fixed regardless of crate
//! choice"): the content comes from Valve's depots and we will never own the
//! producer. So the byte layout below is transcribed from
//! `public/bspfile.h` and is not ours to tidy. What *is* modernized is the
//! mechanism — every record is a `#[repr(C)]` struct that declares its own
//! layout to `bytemuck`, and every read is bounds-checked, in place of
//! `bsplib.cpp`'s casts of raw file offsets to struct pointers.
//!
//! `Cargo.toml` records that `binrw`/`deku` were left out because the formats
//! read so far — the VPK directory, the VTF header — are not struct arrays, and
//! names `.bsp` as the candidate that might change that. It does not: these
//! lumps are plain `Pod` arrays, which `bytemuck` (already a dependency for the
//! uniform blocks) reads without a derive macro or a parser DSL. Revisit for
//! `.mdl`, which has real internal pointers.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::filesystem::Vfs;

/// `IDBSPHEADER` — `'VBSP'` (`public/bspfile.h:23`).
const BSP_IDENT: u32 = u32::from_le_bytes(*b"VBSP");

/// `MINBSPVERSION`/`BSPVERSION` (`public/bspfile.h:26-27`). Portal 2 ships 21.
const MIN_VERSION: i32 = 19;
const MAX_VERSION: i32 = 21;

const HEADER_LUMPS: usize = 64;
const LUMP_ENTRY: usize = 16;
/// `ident` + `m_nVersion` + `lump_t[64]` + `mapRevision`.
const HEADER_SIZE: usize = 4 + 4 + HEADER_LUMPS * LUMP_ENTRY + 4;

// The lumps this reader consumes, from the `enum` at `public/bspfile.h:282`.
// The other 50 are listed there; each arrives with the subsystem that needs it.
const LUMP_ENTITIES: usize = 0;
const LUMP_TEXDATA: usize = 2;
const LUMP_VERTEXES: usize = 3;
const LUMP_TEXINFO: usize = 6;
const LUMP_FACES: usize = 7;
const LUMP_EDGES: usize = 12;
const LUMP_SURFEDGES: usize = 13;
const LUMP_MODELS: usize = 14;
const LUMP_TEXDATA_STRING_DATA: usize = 43;
const LUMP_TEXDATA_STRING_TABLE: usize = 44;

/// Surface flags, from `public/bspflags.h`. Only the ones that decide whether a
/// face is drawn are here; `SURF_BUMPLIGHT` and friends arrive with lightmaps.
pub mod surf {
    /// Don't draw, but add to the skybox.
    pub const SKY: i32 = 0x0004;
    /// Don't draw; sky-lights and draws the 2D sky.
    pub const SKY2D: i32 = 0x0002;
    /// Don't bother referencing the texture.
    pub const NODRAW: i32 = 0x0080;
    /// A primary BSP splitter — a compile-time construct, never drawn.
    pub const HINT: i32 = 0x0100;
    /// Completely ignore, allowing non-closed brushes.
    pub const SKIP: i32 = 0x0200;
    /// An Xbox-era hack that kept trigger surfaces in the tree for occluders.
    pub const TRIGGER: i32 = 0x0040;

    /// Every flag that means "this surface is not world geometry".
    ///
    /// `SKY`/`SKY2D` are in here because the 3D skybox is drawn by a separate
    /// pass over a separate camera that does not exist yet — leaving them in
    /// would drape the map in whatever the sky material resolves to.
    pub const NOT_DRAWN: i32 = SKY | SKY2D | NODRAW | HINT | SKIP | TRIGGER;
}

/// `dedge_t` (`public/bspfile.h:767`).
///
/// Edge 0 is never used: a negative `surfedge` means "this edge, backwards",
/// and zero has no sign.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Edge {
    pub v: [u16; 2],
}

/// `dface_t` (`public/bspfile.h:797`), 56 bytes.
///
/// The `dispinfo`/`surfaceFogVolumeID` pair is a union in spirit and two fields
/// in fact — Valve's own comment says so. `dispinfo >= 0` means the face is a
/// displacement and its geometry lives in the displacement lumps instead.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Face {
    /// Index into `LUMP_PLANES`, which is not read yet — see [`Bsp`].
    pub plane_num: u16,
    /// Faces opposite the node's plane direction.
    pub side: u8,
    pub on_node: u8,
    pub first_edge: i32,
    pub num_edges: i16,
    pub tex_info: i16,
    /// Index into the displacement lumps, or negative for an ordinary face.
    pub disp_info: i16,
    pub surface_fog_volume_id: i16,
    /// `MAXLIGHTMAPS` lightstyles.
    pub styles: [u8; 4],
    /// Byte offset into `LUMP_LIGHTING`, or -1. Stage 5's input.
    pub light_ofs: i32,
    pub area: f32,
    pub lightmap_mins: [i32; 2],
    pub lightmap_size: [i32; 2],
    pub orig_face: i32,
    /// Top bit disables dynamic shadows; the rest is the primitive count
    /// (`dface_t::GetNumPrims`).
    pub num_prims: u16,
    pub first_prim_id: u16,
    pub smoothing_groups: u32,
}

impl Face {
    /// `dface_t::GetNumPrims` (`public/bspfile.h:860`) — the count without the
    /// shadow bit that shares the field.
    pub fn prim_count(&self) -> u16 {
        self.num_prims & 0x7FFF
    }
}

/// `texinfo_t` (`public/bspfile.h:570`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct TexInfo {
    /// `textureVecsTexelsPerWorldUnits[s|t][xyz, offset]` — the plane
    /// projection that turns a world position into a texel coordinate.
    pub texture_vecs: [[f32; 4]; 2],
    /// The same thing for lightmap luxels. Read by stage 5, not by this stage.
    pub lightmap_vecs: [[f32; 4]; 2],
    /// `SURF_*` (`public/bspflags.h`).
    pub flags: i32,
    /// Index into [`Bsp::texdata`].
    pub tex_data: i32,
}

/// `dtexdata_t` (`public/bspfile.h:586`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct TexData {
    pub reflectivity: [f32; 3],
    /// Index into [`Bsp::texdata_string_table`], which indexes the string blob.
    pub name_string_table_id: i32,
    /// The source image's dimensions, recorded by VBSP at compile time. This is
    /// what texture coordinates are divided by — see
    /// [`Bsp::texture_coordinate`].
    pub width: i32,
    pub height: i32,
    pub view_width: i32,
    pub view_height: i32,
}

/// `dmodel_t` (`public/bspfile.h:449`).
///
/// Model 0 is the world. Models 1.. are brush entities — doors, platforms,
/// the moving parts of a test chamber — positioned by the entity that names
/// them, which is why drawing them needs the entity lump and not just this one.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Model {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    /// For sounds and lights, not a render transform.
    pub origin: [f32; 3],
    pub head_node: i32,
    pub first_face: i32,
    pub num_faces: i32,
}

/// Anything that stops a `.bsp` from being read.
#[derive(Debug, thiserror::Error)]
pub enum BspError {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: crate::filesystem::VfsError,
    },

    #[error("{path} is {size} bytes, too short to be a .bsp")]
    TooShort { path: String, size: usize },

    #[error("{path} is not a .bsp file: identifier {ident:#010x}, expected 'VBSP'")]
    NotBsp { path: String, ident: u32 },

    #[error(
        "{path} is .bsp version {version}; this engine reads {} to {}",
        MIN_VERSION,
        MAX_VERSION
    )]
    Version { path: String, version: i32 },

    #[error(
        "lump {lump} of {path} runs past the end of the file \
         (offset {offset}, {length} bytes, file is {size})"
    )]
    LumpOutOfRange {
        path: String,
        lump: usize,
        offset: usize,
        length: usize,
        size: usize,
    },

    /// Console builds LZMA-compress individual lumps and stash the uncompressed
    /// size in the otherwise-unused `fourCC`
    /// (`utils/common/bsplib.cpp:5513`). Consoles are permanently out of scope
    /// (`PORTING.md`, "Supported platforms"), so this is reported rather than
    /// decoded — the alternative is reading compressed bytes as geometry and
    /// drawing noise.
    #[error("lump {lump} of {path} is LZMA-compressed; only PC .bsp files are supported")]
    CompressedLump { path: String, lump: usize },

    #[error(
        "lump {lump} of {path} is {length} bytes, not a whole number of {stride}-byte records"
    )]
    RaggedLump {
        path: String,
        lump: usize,
        length: usize,
        stride: usize,
    },

    #[error("{path} has no {what} lump, so it has no geometry to draw")]
    MissingLump { path: String, what: &'static str },

    #[error("{path} is internally inconsistent: {what}")]
    Corrupt { path: String, what: String },
}

/// One lump's directory entry (`lump_t`, `public/bspfile.h:380`).
#[derive(Debug, Clone, Copy)]
struct LumpEntry {
    offset: usize,
    length: usize,
    #[allow(dead_code)] // read by lumps whose layout changed between versions
    version: i32,
    /// Non-zero means LZMA-compressed; the value is the uncompressed size.
    four_cc: u32,
}

/// A parsed `.bsp`.
///
/// Holds the lumps the renderer walks, decoded into owned `Vec`s — the file's
/// bytes are dropped once this is built. Valve mapped the file and kept
/// pointers into it (`CMapLoadHelper`), which is why so much of `modelloader`
/// is lifetime management by hand.
#[derive(Debug)]
pub struct Bsp {
    /// The path this was read from, for error messages.
    pub path: String,
    pub version: i32,
    /// `mapRevision` — the map's iteration number, as shown by `status`.
    pub revision: i32,
    /// `LUMP_ENTITIES`, still in its on-disk text form. Parse with
    /// [`entities`](Bsp::entities).
    pub entity_lump: String,
    pub vertices: Vec<[f32; 3]>,
    pub edges: Vec<Edge>,
    /// Signed edge references: negative means the edge is walked backwards.
    pub surfedges: Vec<i32>,
    pub faces: Vec<Face>,
    pub texinfo: Vec<TexInfo>,
    pub texdata: Vec<TexData>,
    /// `LUMP_TEXDATA_STRING_TABLE` resolved against `LUMP_TEXDATA_STRING_DATA`:
    /// the material name for each entry, without the `materials/` prefix or the
    /// `.vmt` extension.
    pub texdata_string_table: Vec<String>,
    pub models: Vec<Model>,
}

impl Bsp {
    /// Reads `maps/<name>.bsp` through the game's search paths.
    ///
    /// `name` is the bare map name, as `map` takes it — `sp_a1_intro1`, not
    /// `maps/sp_a1_intro1.bsp`. Any extension or `maps/` prefix given anyway is
    /// stripped, matching `HostState_NewGame`'s `Q_StripExtension`
    /// (`engine/host_state.cpp:134`).
    pub fn load(vfs: &Vfs, name: &str) -> Result<Bsp, BspError> {
        let path = map_path(name);
        let bytes = vfs.read(&path).map_err(|source| BspError::Read {
            path: path.clone(),
            source,
        })?;
        Bsp::parse(path, &bytes)
    }

    /// Parses bytes already in hand. Split out from [`load`](Bsp::load) so the
    /// format can be tested without a mounted game.
    pub fn parse(path: String, bytes: &[u8]) -> Result<Bsp, BspError> {
        if bytes.len() < HEADER_SIZE {
            return Err(BspError::TooShort {
                path,
                size: bytes.len(),
            });
        }

        let ident = u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"));
        if ident != BSP_IDENT {
            return Err(BspError::NotBsp { path, ident });
        }

        let version = i32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
        if !(MIN_VERSION..=MAX_VERSION).contains(&version) {
            return Err(BspError::Version { path, version });
        }

        let mut lumps = [LumpEntry {
            offset: 0,
            length: 0,
            version: 0,
            four_cc: 0,
        }; HEADER_LUMPS];
        for (i, lump) in lumps.iter_mut().enumerate() {
            let at = 8 + i * LUMP_ENTRY;
            let field = |n: usize| {
                i32::from_le_bytes(bytes[at + n..at + n + 4].try_into().expect("4 bytes"))
            };
            // A negative offset or length is corruption; clamping to zero makes
            // the lump read as empty, which the missing-lump checks below catch
            // with a better message than an arithmetic error would.
            *lump = LumpEntry {
                offset: field(0).max(0) as usize,
                length: field(4).max(0) as usize,
                version: field(8),
                four_cc: field(12) as u32,
            };
        }
        let revision = i32::from_le_bytes(
            bytes[8 + HEADER_LUMPS * LUMP_ENTRY..HEADER_SIZE]
                .try_into()
                .expect("4 bytes"),
        );

        let reader = LumpReader {
            path: &path,
            bytes,
            lumps: &lumps,
        };

        let bsp = Bsp {
            entity_lump: reader.text(LUMP_ENTITIES)?,
            vertices: reader.records(LUMP_VERTEXES)?,
            edges: reader.records(LUMP_EDGES)?,
            surfedges: reader.records(LUMP_SURFEDGES)?,
            faces: reader.records(LUMP_FACES)?,
            texinfo: reader.records(LUMP_TEXINFO)?,
            texdata: reader.records(LUMP_TEXDATA)?,
            texdata_string_table: reader.texdata_strings()?,
            models: reader.records(LUMP_MODELS)?,
            path: path.clone(),
            version,
            revision,
        };

        bsp.validate()?;
        Ok(bsp)
    }

    /// Checks the cross-lump references the geometry builder follows, once, so
    /// that walking a face later cannot index out of bounds.
    ///
    /// Valve validated per-lump counts against `MAX_MAP_*` and then trusted the
    /// indices (`Mod_LoadFaces` and friends `Host_Error` on a bad count only).
    /// A `.bsp` is untrusted input — it can come from a downloaded map — so the
    /// references are checked too, and the cost is one pass at load.
    fn validate(&self) -> Result<(), BspError> {
        let corrupt = |what: String| BspError::Corrupt {
            path: self.path.clone(),
            what,
        };

        if self.models.is_empty() {
            return Err(BspError::MissingLump {
                path: self.path.clone(),
                what: "model",
            });
        }
        if self.vertices.is_empty() || self.faces.is_empty() {
            return Err(BspError::MissingLump {
                path: self.path.clone(),
                what: "geometry",
            });
        }

        for (i, edge) in self.edges.iter().enumerate() {
            for v in edge.v {
                if v as usize >= self.vertices.len() {
                    return Err(corrupt(format!(
                        "edge {i} names vertex {v} of {}",
                        self.vertices.len()
                    )));
                }
            }
        }

        for (i, &surfedge) in self.surfedges.iter().enumerate() {
            if surfedge.unsigned_abs() as usize >= self.edges.len() {
                return Err(corrupt(format!(
                    "surfedge {i} names edge {surfedge} of {}",
                    self.edges.len()
                )));
            }
        }

        for (i, face) in self.faces.iter().enumerate() {
            let first = face.first_edge.max(0) as usize;
            let count = face.num_edges.max(0) as usize;
            if face.first_edge < 0 || first + count > self.surfedges.len() {
                return Err(corrupt(format!(
                    "face {i} names surfedges {first}..{} of {}",
                    first + count,
                    self.surfedges.len()
                )));
            }
            if face.tex_info >= 0 && face.tex_info as usize >= self.texinfo.len() {
                return Err(corrupt(format!(
                    "face {i} names texinfo {} of {}",
                    face.tex_info,
                    self.texinfo.len()
                )));
            }
        }

        for (i, info) in self.texinfo.iter().enumerate() {
            if info.tex_data >= 0 && info.tex_data as usize >= self.texdata.len() {
                return Err(corrupt(format!(
                    "texinfo {i} names texdata {} of {}",
                    info.tex_data,
                    self.texdata.len()
                )));
            }
        }

        for (i, model) in self.models.iter().enumerate() {
            let first = model.first_face.max(0) as usize;
            let count = model.num_faces.max(0) as usize;
            if model.first_face < 0 || first + count > self.faces.len() {
                return Err(corrupt(format!(
                    "model {i} names faces {first}..{} of {}",
                    first + count,
                    self.faces.len()
                )));
            }
        }

        Ok(())
    }

    /// The world model — model 0, the static level geometry.
    pub fn world_model(&self) -> &Model {
        &self.models[0] // `validate` rejected an empty model lump
    }

    /// The faces belonging to `model`, in file order.
    pub fn model_faces(&self, model: &Model) -> &[Face] {
        let first = model.first_face as usize;
        &self.faces[first..first + model.num_faces as usize]
    }

    /// The material a face is drawn with, without the `materials/` prefix or
    /// the `.vmt` extension — exactly what
    /// [`MaterialCache::load`](crate::materials::MaterialCache::load) takes.
    ///
    /// `None` when the face has no texinfo, which is legal and means the face
    /// is not drawn.
    pub fn face_material(&self, face: &Face) -> Option<&str> {
        let info = self.texinfo.get(usize::try_from(face.tex_info).ok()?)?;
        let data = self.texdata.get(usize::try_from(info.tex_data).ok()?)?;
        self.texdata_string_table
            .get(usize::try_from(data.name_string_table_id).ok()?)
            .map(String::as_str)
    }

    /// The face's vertices, in winding order.
    ///
    /// This is `Mod_LoadSurfedges`' resolution (`engine/modelloader.cpp:2999`)
    /// done on demand rather than flattened into a `vertindices` array at load:
    /// a negative surfedge means the edge is traversed backwards, so it
    /// contributes its *second* vertex.
    pub fn face_vertices(&self, face: &Face) -> impl Iterator<Item = Vec3> + '_ {
        let first = face.first_edge as usize;
        let count = face.num_edges.max(0) as usize;
        (0..count).map(move |i| {
            let surfedge = self.surfedges[first + i];
            let (edge, end) = if surfedge < 0 {
                (surfedge.unsigned_abs() as usize, 1)
            } else {
                (surfedge as usize, 0)
            };
            Vec3::from(self.vertices[self.edges[edge].v[end] as usize])
        })
    }

    /// The base-texture coordinate for a world position on a face.
    ///
    /// `SurfComputeTextureCoordinate` (`engine/matsys_interface.cpp:1932`): the
    /// texinfo's two plane projections give a coordinate in *texels*, which is
    /// divided by the texture's size to land in the 0..1 the sampler wants.
    ///
    /// **Divergence, deliberate.** Valve divides by the live material's
    /// `GetMappingWidth()`/`GetMappingHeight()` — the `.vtf` actually loaded.
    /// This divides by `dtexdata_t`'s `width`/`height`, the dimensions VBSP
    /// recorded when it compiled the map. For shipped content the two agree,
    /// and the compile-time record has the property the runtime one lacks: it
    /// stays correct when the material falls back to the error checkerboard,
    /// which most Portal 2 world materials do until `LightmappedGeneric` is
    /// written. Dividing by the checkerboard's size instead would scale every
    /// surface in the map by the ratio of the two.
    pub fn texture_coordinate(&self, face: &Face, position: Vec3) -> [f32; 2] {
        let Some(info) = self.texinfo.get(face.tex_info.max(0) as usize) else {
            return [0.0, 0.0];
        };
        let project = |v: [f32; 4]| position.dot(Vec3::new(v[0], v[1], v[2])) + v[3];

        // A zero size would be a divide by zero; VBSP does not emit one, but
        // this is untrusted input and the fallback (texel units) is at least
        // finite.
        let size = self
            .texdata
            .get(info.tex_data.max(0) as usize)
            .map(|d| (d.width as f32, d.height as f32))
            .filter(|&(w, h)| w > 0.0 && h > 0.0)
            .unwrap_or((1.0, 1.0));

        [
            project(info.texture_vecs[0]) / size.0,
            project(info.texture_vecs[1]) / size.1,
        ]
    }

    /// The entity lump, parsed.
    pub fn entities(&self) -> Vec<Entity> {
        parse_entities(&self.entity_lump)
    }
}

/// `maps/<name>.bsp`, however the caller spelled the name.
fn map_path(name: &str) -> String {
    let name = name.replace('\\', "/");
    let name = name.trim_matches('/');
    let name = name.strip_prefix("maps/").unwrap_or(name);
    let name = name.strip_suffix(".bsp").unwrap_or(name);
    format!("maps/{name}.bsp")
}

/// Bounds-checked access to the lump directory.
struct LumpReader<'a> {
    path: &'a str,
    bytes: &'a [u8],
    lumps: &'a [LumpEntry; HEADER_LUMPS],
}

impl LumpReader<'_> {
    fn raw(&self, lump: usize) -> Result<&[u8], BspError> {
        let entry = self.lumps[lump];
        if entry.four_cc != 0 {
            return Err(BspError::CompressedLump {
                path: self.path.to_owned(),
                lump,
            });
        }
        let end = entry.offset.checked_add(entry.length);
        match end {
            Some(end) if end <= self.bytes.len() => Ok(&self.bytes[entry.offset..end]),
            _ => Err(BspError::LumpOutOfRange {
                path: self.path.to_owned(),
                lump,
                offset: entry.offset,
                length: entry.length,
                size: self.bytes.len(),
            }),
        }
    }

    /// A lump that is an array of `T`.
    ///
    /// `pod_read_unaligned` per record rather than `cast_slice` over the whole
    /// lump: a lump begins at an arbitrary file offset, so the alignment
    /// `cast_slice` requires is not guaranteed and it would fail on exactly the
    /// files that happen to be laid out unluckily.
    fn records<T: Pod>(&self, lump: usize) -> Result<Vec<T>, BspError> {
        let bytes = self.raw(lump)?;
        let stride = size_of::<T>();
        if bytes.len() % stride != 0 {
            return Err(BspError::RaggedLump {
                path: self.path.to_owned(),
                lump,
                length: bytes.len(),
                stride,
            });
        }
        Ok(bytes
            .chunks_exact(stride)
            .map(bytemuck::pod_read_unaligned)
            .collect())
    }

    /// A lump that is text. The entity lump is NUL-terminated in the file.
    fn text(&self, lump: usize) -> Result<String, BspError> {
        let bytes = self.raw(lump)?;
        let bytes = match bytes.iter().position(|&b| b == 0) {
            Some(nul) => &bytes[..nul],
            None => bytes,
        };
        // Valve wrote these with `fprintf` and never declared an encoding.
        // Lossy conversion keeps a stray high byte in a mapper's comment from
        // failing the load.
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// `LUMP_TEXDATA_STRING_TABLE` resolved against `LUMP_TEXDATA_STRING_DATA`.
    ///
    /// The table is an array of byte offsets into one NUL-separated blob —
    /// Valve's string interning, so that a material used by a thousand faces is
    /// stored once.
    fn texdata_strings(&self) -> Result<Vec<String>, BspError> {
        let offsets: Vec<i32> = self.records(LUMP_TEXDATA_STRING_TABLE)?;
        let blob = self.raw(LUMP_TEXDATA_STRING_DATA)?;

        Ok(offsets
            .into_iter()
            .map(|offset| {
                let Ok(start) = usize::try_from(offset) else {
                    return String::new();
                };
                if start >= blob.len() {
                    return String::new();
                }
                let rest = &blob[start..];
                let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
                String::from_utf8_lossy(&rest[..end]).into_owned()
            })
            .collect())
    }
}

/// One entity from the entity lump: an unordered bag of key/value strings.
///
/// The lump is the map's entity list as text — `worldspawn`, every light,
/// every trigger, every prop — and the game DLL is what gives most of the keys
/// meaning. Only the two the renderer needs are read here (see
/// [`super::World`]).
#[derive(Debug, Clone, Default)]
pub struct Entity {
    pub pairs: Vec<(String, String)>,
}

impl Entity {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    pub fn classname(&self) -> Option<&str> {
        self.get("classname")
    }

    /// A `"x y z"` value as a vector, in Valve's coordinate order.
    pub fn vector(&self, key: &str) -> Option<Vec3> {
        let mut parts = self.get(key)?.split_ascii_whitespace();
        let mut next = || parts.next()?.parse::<f32>().ok();
        Some(Vec3::new(next()?, next()?, next()?))
    }
}

/// Parses the entity lump's `{ "key" "value" ... }` blocks.
///
/// This is *not* the KeyValues grammar `src/filesystem/keyvalues.rs` reads —
/// it has no nesting, no unquoted tokens and no `[$COND]` suffixes, and
/// `ParseEntities` (`engine/world.cpp`) had its own reader for exactly that
/// reason. Anything malformed ends the parse rather than failing the map load:
/// a map with one bad entity should still open.
fn parse_entities(text: &str) -> Vec<Entity> {
    let mut entities = Vec::new();
    let mut chars = text.char_indices().peekable();
    let bytes = text.as_bytes();

    // Reads one `"..."` token starting at the next quote, or gives up.
    let quoted = |chars: &mut std::iter::Peekable<std::str::CharIndices>| -> Option<String> {
        for (_, c) in chars.by_ref() {
            if c == '"' {
                break;
            }
            // Anything other than whitespace before the quote means this is not
            // a key/value pair — most likely the closing brace.
            if !c.is_whitespace() {
                return None;
            }
        }
        let start = chars.peek()?.0;
        for (i, c) in chars.by_ref() {
            if c == '"' {
                return Some(text[start..i].to_owned());
            }
            if bytes.get(i) == Some(&b'\n') {
                return None;
            }
        }
        None
    };

    while let Some((_, c)) = chars.next() {
        if c != '{' {
            continue;
        }
        let mut entity = Entity::default();
        loop {
            // Peek past whitespace for the closing brace before trying to read
            // a key, so that `}` ends the entity rather than aborting the file.
            let mut closed = false;
            while let Some(&(_, c)) = chars.peek() {
                if c.is_whitespace() {
                    chars.next();
                } else {
                    closed = c == '}';
                    break;
                }
            }
            if closed {
                chars.next();
                break;
            }
            let (Some(key), Some(value)) = (quoted(&mut chars), quoted(&mut chars)) else {
                break;
            };
            entity.pairs.push((key, value));
        }
        if !entity.pairs.is_empty() {
            entities.push(entity);
        }
    }
    entities
}

/// A synthetic `.bsp` with a header, a lump directory, and the lumps `parse`
/// reads. Small enough to reason about: one square face over four vertices.
/// Shared with the geometry builder's tests in [`super`].
#[cfg(test)]
pub(crate) fn one_face_bsp() -> Vec<u8> {
    let mut lumps: Vec<(usize, Vec<u8>)> = Vec::new();

    lumps.push((
        LUMP_ENTITIES,
        b"{\n\"classname\" \"worldspawn\"\n\"skyname\" \"sky_test\"\n}\n\0".to_vec(),
    ));
    lumps.push((
        LUMP_VERTEXES,
        bytemuck::cast_slice::<f32, u8>(&[
            0.0, 0.0, 0.0, //
            64.0, 0.0, 0.0, //
            64.0, 64.0, 0.0, //
            0.0, 64.0, 0.0,
        ])
        .to_vec(),
    ));
    // Edge 0 is the unused one; then the four sides of the square.
    lumps.push((
        LUMP_EDGES,
        bytemuck::cast_slice::<u16, u8>(&[0, 0, 0, 1, 1, 2, 2, 3, 3, 0]).to_vec(),
    ));
    lumps.push((
        LUMP_SURFEDGES,
        bytemuck::cast_slice::<i32, u8>(&[1, 2, 3, 4]).to_vec(),
    ));
    lumps.push((
        LUMP_TEXINFO,
        bytemuck::bytes_of(&TexInfo {
            // One texel per world unit in s, likewise in t.
            texture_vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            lightmap_vecs: [[0.0; 4], [0.0; 4]],
            flags: 0,
            tex_data: 0,
        })
        .to_vec(),
    ));
    lumps.push((
        LUMP_TEXDATA,
        bytemuck::bytes_of(&TexData {
            reflectivity: [0.5, 0.5, 0.5],
            name_string_table_id: 0,
            width: 64,
            height: 64,
            view_width: 64,
            view_height: 64,
        })
        .to_vec(),
    ));
    lumps.push((
        LUMP_TEXDATA_STRING_TABLE,
        bytemuck::cast_slice::<i32, u8>(&[0]).to_vec(),
    ));
    lumps.push((LUMP_TEXDATA_STRING_DATA, b"tools/toolsblack\0".to_vec()));
    lumps.push((
        LUMP_FACES,
        bytemuck::bytes_of(&Face {
            plane_num: 0,
            side: 0,
            on_node: 1,
            first_edge: 0,
            num_edges: 4,
            tex_info: 0,
            disp_info: -1,
            surface_fog_volume_id: -1,
            styles: [0, 255, 255, 255],
            light_ofs: -1,
            area: 4096.0,
            lightmap_mins: [0, 0],
            lightmap_size: [1, 1],
            orig_face: -1,
            num_prims: 0,
            first_prim_id: 0,
            smoothing_groups: 0,
        })
        .to_vec(),
    ));
    lumps.push((
        LUMP_MODELS,
        bytemuck::bytes_of(&Model {
            mins: [0.0, 0.0, 0.0],
            maxs: [64.0, 64.0, 0.0],
            origin: [0.0, 0.0, 0.0],
            head_node: 0,
            first_face: 0,
            num_faces: 1,
        })
        .to_vec(),
    ));

    let mut directory = [[0u8; LUMP_ENTRY]; HEADER_LUMPS];
    let mut body = Vec::new();
    for (id, data) in &lumps {
        let offset = HEADER_SIZE + body.len();
        directory[*id][0..4].copy_from_slice(&(offset as i32).to_le_bytes());
        directory[*id][4..8].copy_from_slice(&(data.len() as i32).to_le_bytes());
        body.extend_from_slice(data);
    }

    let mut file = Vec::new();
    file.extend_from_slice(&BSP_IDENT.to_le_bytes());
    file.extend_from_slice(&21i32.to_le_bytes());
    for entry in &directory {
        file.extend_from_slice(entry);
    }
    file.extend_from_slice(&7i32.to_le_bytes()); // mapRevision
    assert_eq!(file.len(), HEADER_SIZE);
    file.extend_from_slice(&body);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layouts are transcribed from `public/bspfile.h` and every one of
    /// them is a size the file format fixes. A silent change here reads every
    /// subsequent record at the wrong offset, so the sizes are asserted rather
    /// than trusted.
    #[test]
    fn record_sizes_match_the_file_format() {
        assert_eq!(size_of::<Edge>(), 4);
        assert_eq!(size_of::<Face>(), 56);
        assert_eq!(size_of::<TexInfo>(), 72);
        assert_eq!(size_of::<TexData>(), 32);
        assert_eq!(size_of::<Model>(), 48);
        assert_eq!(HEADER_SIZE, 1036);
    }

    #[test]
    fn map_names_are_accepted_however_they_are_spelled() {
        for spelling in [
            "sp_a1_intro1",
            "sp_a1_intro1.bsp",
            "maps/sp_a1_intro1",
            "maps/sp_a1_intro1.bsp",
            "maps\\sp_a1_intro1.bsp",
        ] {
            assert_eq!(map_path(spelling), "maps/sp_a1_intro1.bsp", "{spelling}");
        }
    }

    #[test]
    fn parses_a_whole_file() {
        let bsp = Bsp::parse("test.bsp".into(), &one_face_bsp()).expect("valid");
        assert_eq!(bsp.version, 21);
        assert_eq!(bsp.revision, 7);
        assert_eq!(bsp.faces.len(), 1);
        assert_eq!(bsp.models.len(), 1);
        assert_eq!(bsp.texdata_string_table, ["tools/toolsblack"]);

        let face = &bsp.faces[0];
        assert_eq!(bsp.face_material(face), Some("tools/toolsblack"));
        assert_eq!(bsp.model_faces(bsp.world_model()).len(), 1);

        let verts: Vec<Vec3> = bsp.face_vertices(face).collect();
        assert_eq!(
            verts,
            [
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(64.0, 0.0, 0.0),
                Vec3::new(64.0, 64.0, 0.0),
                Vec3::new(0.0, 64.0, 0.0),
            ]
        );
    }

    #[test]
    fn a_negative_surfedge_walks_its_edge_backwards() {
        // Edge 1 is (vertex 0, vertex 1); -1 must therefore yield vertex 1.
        let mut bytes = one_face_bsp();
        let bsp = Bsp::parse("test.bsp".into(), &bytes).expect("valid");
        let forwards: Vec<Vec3> = bsp.face_vertices(&bsp.faces[0]).collect();

        let surfedges_at = bsp
            .path
            .is_empty()
            .then(|| 0)
            .unwrap_or_else(|| find_lump_offset(&bytes, LUMP_SURFEDGES));
        bytes[surfedges_at..surfedges_at + 4].copy_from_slice(&(-1i32).to_le_bytes());

        let flipped = Bsp::parse("test.bsp".into(), &bytes).expect("valid");
        let backwards: Vec<Vec3> = flipped.face_vertices(&flipped.faces[0]).collect();
        assert_eq!(forwards[0], Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(backwards[0], Vec3::new(64.0, 0.0, 0.0));
    }

    fn find_lump_offset(bytes: &[u8], lump: usize) -> usize {
        let at = 8 + lump * LUMP_ENTRY;
        i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    }

    #[test]
    fn texture_coordinates_are_divided_by_the_texture_size() {
        let bsp = Bsp::parse("test.bsp".into(), &one_face_bsp()).expect("valid");
        let face = &bsp.faces[0];
        // 64 world units at one texel per unit over a 64-texel texture is
        // exactly one repeat.
        assert_eq!(
            bsp.texture_coordinate(face, Vec3::new(64.0, 32.0, 0.0)),
            [1.0, 0.5]
        );
    }

    #[test]
    fn a_compressed_lump_is_reported_rather_than_read_as_geometry() {
        let mut bytes = one_face_bsp();
        // Stamp an uncompressed size into LUMP_FACES' fourCC.
        let at = 8 + LUMP_FACES * LUMP_ENTRY + 12;
        bytes[at..at + 4].copy_from_slice(&4096i32.to_le_bytes());
        assert!(matches!(
            Bsp::parse("test.bsp".into(), &bytes),
            Err(BspError::CompressedLump { .. })
        ));
    }

    #[test]
    fn a_wrong_identifier_or_version_is_refused() {
        let mut bytes = one_face_bsp();
        bytes[0..4].copy_from_slice(b"XBSP");
        assert!(matches!(
            Bsp::parse("t.bsp".into(), &bytes),
            Err(BspError::NotBsp { .. })
        ));

        let mut bytes = one_face_bsp();
        bytes[4..8].copy_from_slice(&18i32.to_le_bytes());
        assert!(matches!(
            Bsp::parse("t.bsp".into(), &bytes),
            Err(BspError::Version { .. })
        ));
    }

    #[test]
    fn a_face_naming_a_vertex_that_is_not_there_is_caught_at_load() {
        let mut bytes = one_face_bsp();
        let faces_at = find_lump_offset(&bytes, LUMP_FACES);
        // num_edges lives at offset 8 within dface_t.
        bytes[faces_at + 8..faces_at + 10].copy_from_slice(&999i16.to_le_bytes());
        assert!(matches!(
            Bsp::parse("t.bsp".into(), &bytes),
            Err(BspError::Corrupt { .. })
        ));
    }

    #[test]
    fn entities_are_parsed_into_key_value_bags() {
        let entities = parse_entities(
            "{\n\"classname\" \"worldspawn\"\n\"skyname\" \"sky_day01_01\"\n}\n\
             {\n\"classname\" \"info_player_start\"\n\"origin\" \"-1024 512 64\"\n\
             \"angles\" \"0 90 0\"\n}\n",
        );
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].classname(), Some("worldspawn"));
        assert_eq!(entities[0].get("skyname"), Some("sky_day01_01"));
        assert_eq!(entities[1].classname(), Some("info_player_start"));
        assert_eq!(
            entities[1].vector("origin"),
            Some(Vec3::new(-1024.0, 512.0, 64.0))
        );
        assert_eq!(
            entities[1].vector("angles"),
            Some(Vec3::new(0.0, 90.0, 0.0))
        );
    }

    #[test]
    fn a_malformed_entity_does_not_lose_the_ones_before_it() {
        let entities = parse_entities("{\n\"classname\" \"worldspawn\"\n}\n{\n\"unterminated");
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].classname(), Some("worldspawn"));
    }

    #[test]
    fn the_entity_lump_survives_the_round_trip_through_a_file() {
        let bsp = Bsp::parse("test.bsp".into(), &one_face_bsp()).expect("valid");
        let entities = bsp.entities();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].get("skyname"), Some("sky_test"));
    }
}
