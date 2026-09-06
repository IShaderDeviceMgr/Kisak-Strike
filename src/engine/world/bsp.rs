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
use crate::materials::lightmap::ColorRgbExp32;

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
/// The collision lumps. `trace/` is their only consumer; they are read here
/// because this is the `.bsp` reader and Valve's second one
/// (`engine/cmodel_bsp.cpp`) existed only because collision lived in code that
/// could not see `modelloader.cpp`'s allocations. See
/// `portdocs/ENGINE_TRACE.md` §7.4.
const LUMP_PLANES: usize = 1;
const LUMP_TEXDATA: usize = 2;
const LUMP_VERTEXES: usize = 3;
const LUMP_NODES: usize = 5;
const LUMP_TEXINFO: usize = 6;
const LUMP_FACES: usize = 7;
const LUMP_LIGHTING: usize = 8;
const LUMP_LEAFS: usize = 10;
const LUMP_EDGES: usize = 12;
const LUMP_SURFEDGES: usize = 13;
const LUMP_MODELS: usize = 14;
const LUMP_LEAFBRUSHES: usize = 17;
const LUMP_BRUSHES: usize = 18;
const LUMP_BRUSHSIDES: usize = 19;
const LUMP_GAME_LUMP: usize = 35;
const LUMP_LEAF_AMBIENT_INDEX_HDR: usize = 51;
const LUMP_LEAF_AMBIENT_INDEX: usize = 52;
const LUMP_LEAF_AMBIENT_LIGHTING_HDR: usize = 55;
const LUMP_LEAF_AMBIENT_LIGHTING: usize = 56;
const LUMP_TEXDATA_STRING_DATA: usize = 43;
const LUMP_TEXDATA_STRING_TABLE: usize = 44;
const LUMP_LIGHTING_HDR: usize = 53;
const LUMP_FACES_HDR: usize = 58;
const LUMP_MAP_FLAGS: usize = 59;

/// `LVLFLAGS_LIGHTMAP_ALPHA` (`public/bspfile.h:400`) — "indicates that
/// lightmap alpha data is interleved in the lighting lump".
///
/// The CS:GO-era cascaded-shadow-map term, one byte per luxel, written after
/// each lightstyle's colour samples. Cascaded shadow maps are not ported, but
/// the *stride* matters regardless: with this set, every face's samples start
/// somewhere different. Portal 2 does not set it —`sp_a1_intro1`'s
/// `LUMP_MAP_FLAGS` is 2, `LVLFLAGS_BAKED_STATIC_PROP_LIGHTING_HDR` alone — so
/// a map that does is refused rather than misread. See
/// [`BspError::UnsupportedLightmapAlpha`].
const LVLFLAGS_LIGHTMAP_ALPHA: u32 = 0x0000_0004;
/// `LVLFLAGS_LIGHTMAP_ALPHA_3` — three sets of the above.
const LVLFLAGS_LIGHTMAP_ALPHA_3: u32 = 0x0000_0010;

/// `LUMP_LEAFS_VERSION` (`public/bspfile.h:370`). Version 0 carries a
/// `CompressedLightCube` inline and is 56 bytes a leaf; version 1 moved that to
/// `LUMP_LEAF_AMBIENT_LIGHTING` and is 32. Portal 2 ships version 1, and the two
/// are indistinguishable except by this field — so a version 0 map is refused
/// rather than read at the wrong stride, which would produce a plausible tree
/// of nonsense rather than an error. See [`BspError::UnsupportedLeafVersion`].
const LEAFS_VERSION: i32 = 1;

/// `MAXLIGHTMAPS` (`public/bspfile.h:773`): how many lightstyles one face can
/// carry. A style of 255 means "no more".
const MAX_LIGHTMAPS: usize = 4;
const NO_LIGHTSTYLE: u8 = 255;

/// Surface flags, from `public/bspflags.h`. Only the ones this reader acts on.
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

    /// Don't calculate light: the surface has no lightmap and binds the white
    /// page. `SurfHasLightmap` (`engine/gl_matsysiface.cpp:573`) tests it.
    pub const NOLIGHT: i32 = 0x0400;
    /// The surface's lighting lump entry holds four blocks — the flat lightmap
    /// and one per bump-basis vector — rather than one.
    ///
    /// Set by VBSP when it compiled the map against a material with a
    /// `$bumpmap`. **Valve's engine never reads it for this**: it re-derives
    /// the same answer at load from the live material
    /// (`SurfNeedsBumpedLightmaps`, `gl_matsysiface.cpp:565`), so a material
    /// edited after the map was compiled makes the engine read the lighting
    /// lump with the wrong stride. Reading the flag instead is both simpler and
    /// right by construction, because it is the file describing its own layout.
    ///
    /// Checked against the data on `sp_a1_intro1`: over all 4,982 lit faces the
    /// flag agrees with the byte spacing between consecutive light offsets,
    /// 4,982 to 0.
    pub const BUMPLIGHT: i32 = 0x0800;

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

/// `dplane_t` (`public/bspfile.h:550`), 20 bytes.
///
/// A brush side and a BSP node both name one of these. `dist` is measured along
/// `normal` from the origin, so a point is in front when
/// `normal.dot(point) - dist > 0`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Plane {
    pub normal: [f32; 3],
    pub dist: f32,
    /// `PLANE_X`/`Y`/`Z` (0-2) when the normal is axial, `PLANE_ANYX`/`Y`/`Z`
    /// (3-5) otherwise.
    ///
    /// Valve's comment calls this "trivial to regenerate" and it is, but the
    /// trace reads it on every node it descends: an axial plane needs one
    /// subtraction where a general one needs a dot product, and the box's
    /// extent along an axial normal is one component rather than three
    /// `fabsf`s (`engine/cmodel.cpp:2578`).
    pub plane_type: i32,
}

impl Plane {
    /// Whether [`plane_type`](Plane::plane_type) names an axis, in which case
    /// it is also the index of that axis.
    pub fn is_axial(&self) -> bool {
        (0..3).contains(&self.plane_type)
    }
}

/// `dnode_t` (`public/bspfile.h:562`), 32 bytes.
///
/// `children` is the whole BSP: a non-negative entry is another node, and a
/// negative one is the leaf at `-1 - child`. Child 0 is in front of the plane.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Node {
    pub plane_num: i32,
    /// Front, then back. Negative means `-1 - child` indexes [`Bsp::leaves`].
    pub children: [i32; 2],
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub first_face: u16,
    pub num_faces: u16,
    /// The area every leaf below this node is in, or -1 when they differ.
    pub area: i16,
    /// `dnode_t` is 30 bytes of fields with 4-byte alignment, so the compiler
    /// that wrote the file put two bytes here. Named because `bytemuck::Pod`
    /// refuses a type with implicit padding — reading it is what proves the
    /// stride is 32.
    pub _pad: u16,
}

/// `dleaf_t` version 1 (`public/bspfile.h:930`), 32 bytes.
///
/// The trace reads `contents` — to reject a whole leaf before looking at a
/// brush — and the leaf-brush range. The rest is visibility's
/// (`portdocs/ENGINE_TRACE.md` §1).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Leaf {
    /// The OR of every brush in the leaf (`CONTENTS_*`).
    pub contents: i32,
    /// The PVS cluster, or -1 for a leaf that sees nothing.
    pub cluster: i16,
    /// Valve's `short area:9; short flags:7;` bitfield in one field.
    ///
    /// Kept packed rather than split: bit order in a C bitfield is
    /// implementation-defined, the two halves are visibility's and not this
    /// module's, and unpacking them here would be inventing a guarantee the
    /// format does not make. `area()` and `flags()` decode the layout every
    /// compiler that has ever built this actually used — LSB first.
    pub area_flags: u16,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub first_leaf_face: u16,
    pub num_leaf_faces: u16,
    pub first_leaf_brush: u16,
    pub num_leaf_brushes: u16,
    /// -1 when the leaf is not in water.
    pub leaf_water_data_id: i16,
    /// Trailing padding to the 32-byte stride — see [`Node::_pad`].
    pub _pad: u16,
}

impl Leaf {
    // Visibility's, not the trace's: `world/`'s PVS work is the first caller
    // (`portdocs/ENGINE_TRACE.md` §1). Kept here because this is where the
    // packing is documented.
    #[allow(dead_code)]
    /// The `area:9` half of [`area_flags`](Leaf::area_flags).
    pub fn area(&self) -> u16 {
        self.area_flags & 0x01FF
    }

    #[allow(dead_code)]
    /// The `flags:7` half.
    pub fn flags(&self) -> u16 {
        self.area_flags >> 9
    }
}

/// `dbrush_t` (`public/bspfile.h:995`), 12 bytes.
///
/// A convex volume: the intersection of the half-spaces named by its sides.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Brush {
    pub first_side: i32,
    pub num_sides: i32,
    /// `CONTENTS_*` (`public/bspflags.h`) — what this volume is made of.
    pub contents: i32,
}

/// `dbrushside_t` (`public/bspfile.h:985`), 8 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct BrushSide {
    /// Index into [`Bsp::planes`], facing *out* of the brush.
    pub plane_num: u16,
    pub tex_info: i16,
    /// Index into the displacement lumps, or negative.
    pub disp_info: i16,
    /// Non-zero for a plane `vbsp` added so that a swept *box* clips exactly.
    /// Point traces must skip these; box traces must not
    /// (`portdocs/ENGINE_TRACE.md` §4.4).
    pub bevel: u8,
    /// A CS:GO-era addition; see `portdocs/ENGINE_TRACE.md` §9.1.
    pub thin: u8,
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

    #[error(
        "{path} has interleaved lightmap alpha data (level flags {level_flags:#010x}); \
         only maps without it are supported"
    )]
    UnsupportedLightmapAlpha { path: String, level_flags: u32 },

    #[error("{path} has no {what} lump, so it has no geometry to draw")]
    MissingLump { path: String, what: &'static str },

    #[error(
        "{path} has version {version} leaves; this engine reads version {}",
        LEAFS_VERSION
    )]
    UnsupportedLeafVersion { path: String, version: i32 },

    #[error("{path} is internally inconsistent: {what}")]
    Corrupt { path: String, what: String },
}

/// One lump's directory entry (`lump_t`, `public/bspfile.h:380`).
#[derive(Debug, Clone, Copy)]
struct LumpEntry {
    offset: usize,
    length: usize,
    /// Read by lumps whose layout changed between versions — `LUMP_LEAFS` is
    /// the one this reader acts on.
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
    /// The collision lumps. Read here, given meaning by
    /// [`trace`](crate::engine::trace) — see `portdocs/ENGINE_TRACE.md` §7.4.
    ///
    /// **All six may legitimately be empty.** A `.bsp` with no brushes is not
    /// corrupt, it is a map nothing can collide with, and `CM_BoxTrace`'s own
    /// first act is to return the cleared trace when `numnodes` is 0
    /// (`engine/cmodel.cpp:3208`). So these are not checked by
    /// [`validate`](Bsp::validate) for presence, only for consistency.
    pub planes: Vec<Plane>,
    pub nodes: Vec<Node>,
    pub leaves: Vec<Leaf>,
    /// `LUMP_LEAFBRUSHES` — the brush indices each leaf's range points into.
    pub leaf_brushes: Vec<u16>,
    pub brushes: Vec<Brush>,
    pub brush_sides: Vec<BrushSide>,
    /// `LUMP_LIGHTING_HDR` if the map has one, else `LUMP_LIGHTING`: the baked
    /// light samples every lit face indexes with its `light_ofs`.
    ///
    /// Empty for a map compiled without `vrad`, which is legal and draws
    /// fullbright.
    pub lighting: Vec<ColorRgbExp32>,
    /// Which of the two lighting lumps [`lighting`](Bsp::lighting) came from.
    ///
    /// Recorded because it decides the exposure the samples are in, not just
    /// where they were read: LDR samples are pre-divided by the overbright
    /// factor and HDR ones are not. **Portal 2 ships HDR-only maps** — this is
    /// always true for them — and the LDR case is here for maps that are not
    /// Portal 2's.
    pub lighting_is_hdr: bool,
    /// `LUMP_MAP_FLAGS`' `m_LevelFlags` (`dflagslump_t`), or 0 when the lump is
    /// absent, which it is on maps older than this feature.
    ///
    /// Read at parse time for the lightmap-alpha bits and kept because the
    /// rest of it is what `Map_CheckFeatureFlags` (`modelloader.cpp:1178`)
    /// hands to the subsystems that arrive later —
    /// `LVLFLAGS_BAKED_STATIC_PROP_LIGHTING_HDR` to static props,
    /// `LVLFLAGS_LIGHTSTYLES_WITH_CSM` to the light-style animator.
    #[allow(dead_code)]
    pub level_flags: u32,
    /// `LUMP_GAME_LUMP`'s directory, payloads included. Empty for a map with
    /// no game lumps, which is legal.
    pub game_lumps: Vec<GameLump>,
    /// `LUMP_LEAF_AMBIENT_LIGHTING_HDR` or its LDR twin — the baked ambient
    /// cubes every model in the level is lit by. Empty on a map compiled
    /// without `vrad`.
    ///
    /// Which lump this came from follows
    /// [`lighting_is_hdr`](Bsp::lighting_is_hdr), for the same reason and by
    /// the same rule.
    pub leaf_ambient: Vec<LeafAmbientSample>,
    /// `LUMP_LEAF_AMBIENT_INDEX*` — parallel to [`leaves`](Bsp::leaves).
    pub leaf_ambient_index: Vec<LeafAmbientIndex>,
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

        // `Map_CheckFeatureFlags` (`engine/modelloader.cpp:1178`). Read before
        // anything else, because it says how the lighting lump is laid out.
        let level_flags = match reader.raw(LUMP_MAP_FLAGS) {
            Ok(bytes) if bytes.len() >= 4 => {
                u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"))
            }
            _ => 0,
        };
        if level_flags & (LVLFLAGS_LIGHTMAP_ALPHA | LVLFLAGS_LIGHTMAP_ALPHA_3) != 0 {
            return Err(BspError::UnsupportedLightmapAlpha { path, level_flags });
        }

        // `Mod_LoadFaces` picks `LUMP_FACES_HDR` whenever HDR is on and the
        // lump is non-empty (`modelloader.cpp:2188`), because the HDR faces
        // carry different `light_ofs` values — and, in an HDR-only map,
        // *only* the HDR ones are meaningful: `sp_a1_intro1`'s LDR faces all
        // have `light_ofs` 0 against an empty `LUMP_LIGHTING`.
        let lighting_is_hdr = !reader.is_empty(LUMP_LIGHTING_HDR);
        let faces_lump = if lighting_is_hdr && !reader.is_empty(LUMP_FACES_HDR) {
            LUMP_FACES_HDR
        } else {
            LUMP_FACES
        };
        let lighting_lump = if lighting_is_hdr {
            LUMP_LIGHTING_HDR
        } else {
            LUMP_LIGHTING
        };

        // The leaf lump is the one collision lump whose *stride* changed
        // between versions, and the directory is the only place that says
        // which. Refuse before `records` reads 56-byte leaves as 32-byte ones.
        let leafs_version = reader.version(LUMP_LEAFS);
        if !reader.is_empty(LUMP_LEAFS) && leafs_version != LEAFS_VERSION {
            return Err(BspError::UnsupportedLeafVersion {
                path,
                version: leafs_version,
            });
        }

        let bsp = Bsp {
            entity_lump: reader.text(LUMP_ENTITIES)?,
            lighting: reader.records(lighting_lump)?,
            lighting_is_hdr,
            level_flags,
            game_lumps: reader.game_lumps()?,
            leaf_ambient: reader.records(if lighting_is_hdr {
                LUMP_LEAF_AMBIENT_LIGHTING_HDR
            } else {
                LUMP_LEAF_AMBIENT_LIGHTING
            })?,
            leaf_ambient_index: reader.records(if lighting_is_hdr {
                LUMP_LEAF_AMBIENT_INDEX_HDR
            } else {
                LUMP_LEAF_AMBIENT_INDEX
            })?,
            vertices: reader.records(LUMP_VERTEXES)?,
            edges: reader.records(LUMP_EDGES)?,
            surfedges: reader.records(LUMP_SURFEDGES)?,
            faces: reader.records(faces_lump)?,
            texinfo: reader.records(LUMP_TEXINFO)?,
            texdata: reader.records(LUMP_TEXDATA)?,
            texdata_string_table: reader.texdata_strings()?,
            models: reader.records(LUMP_MODELS)?,
            planes: reader.records(LUMP_PLANES)?,
            nodes: reader.records(LUMP_NODES)?,
            leaves: reader.records(LUMP_LEAFS)?,
            leaf_brushes: reader.records(LUMP_LEAFBRUSHES)?,
            brushes: reader.records(LUMP_BRUSHES)?,
            brush_sides: reader.records(LUMP_BRUSHSIDES)?,
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
            if model.head_node < 0 && !self.nodes.is_empty() {
                return Err(corrupt(format!(
                    "model {i} has head node {}, which is not a node",
                    model.head_node
                )));
            }
        }

        // The collision lumps. Checked here for the same reason the render
        // lumps are: the trace descends this tree per frame per player and
        // indexes it directly, so one pass at load buys the right to do that
        // without a bounds check in the inner loop.
        for (i, node) in self.nodes.iter().enumerate() {
            if node.plane_num < 0 || node.plane_num as usize >= self.planes.len() {
                return Err(corrupt(format!(
                    "node {i} names plane {} of {}",
                    node.plane_num,
                    self.planes.len()
                )));
            }
            for child in node.children {
                let ok = if child < 0 {
                    // `-1 - child`, so -1 is leaf 0.
                    ((-1 - child) as usize) < self.leaves.len()
                } else {
                    (child as usize) < self.nodes.len()
                };
                if !ok {
                    return Err(corrupt(format!(
                        "node {i} names child {child}, which is neither a node of {} \
                         nor a leaf of {}",
                        self.nodes.len(),
                        self.leaves.len()
                    )));
                }
            }
        }

        for (i, leaf) in self.leaves.iter().enumerate() {
            let first = leaf.first_leaf_brush as usize;
            let end = first + leaf.num_leaf_brushes as usize;
            if end > self.leaf_brushes.len() {
                return Err(corrupt(format!(
                    "leaf {i} names leafbrushes {first}..{end} of {}",
                    self.leaf_brushes.len()
                )));
            }
        }

        for (i, &brush) in self.leaf_brushes.iter().enumerate() {
            if brush as usize >= self.brushes.len() {
                return Err(corrupt(format!(
                    "leafbrush {i} names brush {brush} of {}",
                    self.brushes.len()
                )));
            }
        }

        for (i, brush) in self.brushes.iter().enumerate() {
            let first = brush.first_side.max(0) as usize;
            let end = first + brush.num_sides.max(0) as usize;
            if brush.first_side < 0 || brush.num_sides < 0 || end > self.brush_sides.len() {
                return Err(corrupt(format!(
                    "brush {i} names sides {first}..{end} of {}",
                    self.brush_sides.len()
                )));
            }
        }

        for (i, side) in self.brush_sides.iter().enumerate() {
            if side.plane_num as usize >= self.planes.len() {
                return Err(corrupt(format!(
                    "brush side {i} names plane {} of {}",
                    side.plane_num,
                    self.planes.len()
                )));
            }
            if side.tex_info >= 0 && side.tex_info as usize >= self.texinfo.len() {
                return Err(corrupt(format!(
                    "brush side {i} names texinfo {} of {}",
                    side.tex_info,
                    self.texinfo.len()
                )));
            }
        }

        Ok(())
    }

    /// One game lump by its four-CC id, e.g. [`GAMELUMP_STATIC_PROPS`].
    ///
    /// [`GAMELUMP_STATIC_PROPS`]: super::props::GAMELUMP_STATIC_PROPS
    pub fn game_lump(&self, id: u32) -> Option<&GameLump> {
        self.game_lumps.iter().find(|lump| lump.id == id)
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

    /// How many lightstyles a face carries, and therefore how many copies of
    /// its samples the lighting lump holds.
    ///
    /// `for ( maps = 0; maps < MAXLIGHTMAPS && styles[maps] != 255; ++maps )`
    /// (`gl_lightmap.cpp:1366`). Style 0 is the always-on one; the rest are
    /// switchable lights, which are not animated here — see
    /// [`face_lightmap_samples`](Bsp::face_lightmap_samples).
    pub fn face_lightstyle_count(face: &Face) -> usize {
        face.styles
            .iter()
            .position(|&style| style == NO_LIGHTSTYLE)
            .unwrap_or(MAX_LIGHTMAPS)
    }

    /// How many lightmap blocks a face's samples occupy: 4 when the surface
    /// was compiled bumped, else 1. See [`surf::BUMPLIGHT`].
    pub fn face_lightmap_blocks(&self, face: &Face) -> u32 {
        let bumped = self
            .texinfo
            .get(face.tex_info.max(0) as usize)
            .is_some_and(|info| info.flags & surf::BUMPLIGHT != 0);
        if bumped {
            crate::materials::lightmap::BUMP_BLOCKS
        } else {
            1
        }
    }

    /// The face's lightmap dimensions in luxels.
    ///
    /// `MSurf_LightmapExtents + 1` (`gl_matsysiface.cpp:223`): `lightmap_size`
    /// is the extent, so a face whose light varies over a single luxel records
    /// zero.
    pub fn face_lightmap_size(face: &Face) -> (u32, u32) {
        (
            (face.lightmap_size[0].max(0) as u32) + 1,
            (face.lightmap_size[1].max(0) as u32) + 1,
        )
    }

    /// The face's baked light samples, or `None` if it has none.
    ///
    /// The returned slice is `blocks * width * height` samples of **lightstyle
    /// 0 only**. The other styles follow it in the lump and are deliberately
    /// not returned: they are the switchable and animated lights, and summing
    /// them needs `LightStyleValue( style )` from a light-style animator that
    /// does not exist (`R_BuildLightMap`, `gl_lightmap.cpp:1623`, is a
    /// per-frame rebuild of the whole page). Style 0 is what a map looks like
    /// with every switchable light in its compiled-in state, which is what
    /// `vrad` bakes it as.
    ///
    /// `light_ofs` points *past* the per-style average colours — one
    /// `ColorRGBExp32` per style — that `vrad` writes ahead of the samples, so
    /// no adjustment is needed here. Verified against `sp_a1_intro1`, where
    /// consecutive faces' offsets differ by exactly the sample bytes plus the
    /// next face's average colours.
    pub fn face_lightmap_samples(&self, face: &Face) -> Option<&[ColorRgbExp32]> {
        if face.light_ofs < 0 || self.lighting.is_empty() {
            return None;
        }
        let info = self.texinfo.get(face.tex_info.max(0) as usize)?;
        if info.flags & surf::NOLIGHT != 0 {
            return None;
        }
        let (width, height) = Bsp::face_lightmap_size(face);
        let count = (self.face_lightmap_blocks(face) * width * height) as usize;
        // `light_ofs` is a byte offset into a lump of 4-byte samples.
        let first = (face.light_ofs as usize).checked_div(size_of::<ColorRgbExp32>())?;
        self.lighting.get(first..first.checked_add(count)?)
    }

    /// The lightmap coordinate for a world position on a face, in **luxels**.
    ///
    /// `SurfComputeLightmapCoordinate` (`engine/matsys_interface.cpp:1956`)
    /// without its final scale into page space, which the caller applies
    /// because only it knows which page the face landed on:
    ///
    /// ```text
    /// uv = dot( pos, lightmapVecs[i].xyz ) + lightmapVecs[i][3]
    ///      - lightmapMins[i] + 0.5
    /// ```
    ///
    /// The `+ 0.5` is the half-luxel that puts the coordinate at the *centre*
    /// of the first luxel rather than its corner; without it every lit surface
    /// is offset by half a luxel and the bilinear filter samples across the
    /// block boundary into whatever the packer put next door.
    pub fn lightmap_coordinate(&self, face: &Face, position: Vec3) -> [f32; 2] {
        let Some(info) = self.texinfo.get(face.tex_info.max(0) as usize) else {
            return [0.5, 0.5];
        };
        let luxel = |axis: usize| {
            let v = info.lightmap_vecs[axis];
            position.dot(Vec3::new(v[0], v[1], v[2])) + v[3] - face.lightmap_mins[axis] as f32 + 0.5
        };
        [luxel(0), luxel(1)]
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

/// One baked ambient sample: a light cube and where in its leaf it was taken.
///
/// `dleafambientlighting_t` (`bspfile.h:967`). `vrad` places several of these
/// per leaf and `Mod_LeafAmbientColorAtPos` interpolates between them, which is
/// why a big room does not light every model in it identically.
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct LeafAmbientSample {
    /// `CompressedLightCube` — light arriving from `+x, -x, +y, -y, +z, -z`,
    /// in that order, which is the order
    /// [`ModelLighting::ambient_cube`](crate::materials::uniforms::ModelLighting::ambient_cube)
    /// wants.
    pub cube: [ColorRgbExp32; 6],
    /// The sample's position as a fixed-point fraction of the leaf's bounds:
    /// `mins + (xyz / 255) * (maxs - mins)`. See
    /// [`Bsp::leaf_ambient_at`].
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub _pad: u8,
}

/// One leaf's slice of [`Bsp::leaf_ambient`].
///
/// `dleafambientindex_t` (`bspfile.h:977`). **A zero count with a non-zero
/// first sample is not an empty leaf** — it is a *solid* leaf borrowing another
/// leaf's samples, and `firstAmbientSample` is that leaf's index rather than a
/// sample index. Reading it the other way leaves every prop embedded in
/// geometry unlit. `modelloader.cpp:7309`.
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct LeafAmbientIndex {
    pub sample_count: u16,
    pub first_sample: u16,
}

/// One entry of `LUMP_GAME_LUMP`'s directory, with its payload copied out.
///
/// A game lump is a lump *inside* a lump: `LUMP_GAME_LUMP` holds a count and a
/// `dgamelump_t[]` directory (`bspfile.h:437`), and each entry names a range
/// somewhere else in the file. It exists because the lump directory is a fixed
/// 64 entries and a game DLL cannot claim one — so `sprp` (static props) and
/// `dprp` (detail props) live here instead, and a mod could add its own.
///
/// The payload is copied rather than borrowed for the same reason every other
/// lump is: a [`Bsp`] outlives the bytes it was parsed from.
#[derive(Debug, Clone)]
pub struct GameLump {
    /// A four-CC read as a little-endian `i32`, so `'sprp'` is `0x73707270`.
    pub id: u32,
    /// `GAMELUMPFLAG_COMPRESSED` is the only defined bit and is X360-only, so
    /// nothing on this port's POSIX-only path reads it.
    #[allow(dead_code)]
    pub flags: u16,
    /// Versioned independently of the `.bsp` — Portal 2's `sprp` is 9 while
    /// the file around it is 21.
    pub version: u16,
    pub data: Vec<u8>,
}

/// Bounds-checked access to the lump directory.
struct LumpReader<'a> {
    path: &'a str,
    bytes: &'a [u8],
    lumps: &'a [LumpEntry; HEADER_LUMPS],
}

impl LumpReader<'_> {
    /// Whether a lump is absent or zero-length, which the directory does not
    /// distinguish and neither does anything that asks.
    fn is_empty(&self, lump: usize) -> bool {
        self.lumps[lump].length == 0
    }

    /// The directory's per-lump version field.
    fn version(&self, lump: usize) -> i32 {
        self.lumps[lump].version
    }

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

    /// `LUMP_GAME_LUMP`'s directory, with each entry's payload copied out.
    ///
    /// Two things about this lump are not like any other. **A game lump's
    /// `fileofs` is absolute in the file**, not relative to `LUMP_GAME_LUMP` —
    /// the payloads usually sit inside the game lump's own range, but nothing
    /// requires it and the field does not mean what its name suggests. And
    /// **`filelen` is unreliable on the last entry**: Valve's own X360
    /// compression path documents deriving a lump's size from *the next
    /// entry's* `fileofs` (`bspfile.h:433`), and some compilers write a
    /// terminating entry with a zero length to make that work. Both are handled
    /// by clamping to the file rather than trusting the field.
    fn game_lumps(&self) -> Result<Vec<GameLump>, BspError> {
        let dir = self.raw(LUMP_GAME_LUMP)?;
        if dir.len() < 4 {
            // No game lumps at all. Legal — a map with no props has none.
            return Ok(Vec::new());
        }
        let count = i32::from_le_bytes(dir[0..4].try_into().expect("4 bytes"));
        let count = usize::try_from(count).map_err(|_| BspError::Corrupt {
            path: self.path.to_owned(),
            what: format!("the game lump directory declares {count} lumps"),
        })?;

        const ENTRY: usize = 16;
        if dir.len() < 4 + count * ENTRY {
            return Err(BspError::Corrupt {
                path: self.path.to_owned(),
                what: format!(
                    "the game lump directory declares {count} lumps but is {} bytes",
                    dir.len()
                ),
            });
        }

        let mut lumps = Vec::with_capacity(count);
        for i in 0..count {
            let at = 4 + i * ENTRY;
            let field = |n: usize| {
                i32::from_le_bytes(dir[at + n..at + n + 4].try_into().expect("4 bytes"))
            };
            let half = |n: usize| u16::from_le_bytes(dir[at + n..at + n + 2].try_into().expect("2"));

            let id = field(0) as u32;
            let flags = half(4);
            let version = half(6);
            let offset = field(8).max(0) as usize;
            let length = field(12).max(0) as usize;

            // A terminating entry — id 0, or a range that is not in the file —
            // carries no payload and is dropped rather than refused.
            let end = offset.saturating_add(length);
            let data = if id == 0 || length == 0 || end > self.bytes.len() {
                Vec::new()
            } else {
                self.bytes[offset..end].to_vec()
            };
            if id == 0 {
                continue;
            }
            lumps.push(GameLump {
                id,
                flags,
                version,
                data,
            });
        }
        Ok(lumps)
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
    face_bsp(None)
}

/// The same map with HDR lighting for its one face: a 2x2 lightmap, or four
/// 2x2 blocks when `bumped`, over the 64-unit square.
///
/// `light_ofs` points past one average-colour sample, which is how `vrad`
/// writes it — see [`Bsp::face_lightmap_samples`].
#[cfg(test)]
pub(crate) fn lit_face_bsp(bumped: bool) -> Vec<u8> {
    face_bsp(Some(bumped))
}

#[cfg(test)]
fn face_bsp(lit: Option<bool>) -> Vec<u8> {
    let bumped = lit == Some(true);
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
            // One luxel per 32 units, so the 64-unit square spans 0..2 luxels
            // and its `lightmap_size` extent of 1 is right.
            lightmap_vecs: [[1.0 / 32.0, 0.0, 0.0, 0.0], [0.0, 1.0 / 32.0, 0.0, 0.0]],
            flags: if bumped { surf::BUMPLIGHT } else { 0 },
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
            // Past the one average-colour sample `vrad` writes per style.
            light_ofs: if lit.is_some() { 4 } else { -1 },
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
    if lit.is_some() {
        // One average colour, then 4 luxels per block. Each block is a
        // distinguishable flat grey: 1, 2, 3, 4 at exponent 0.
        let blocks = if bumped { 4 } else { 1 };
        let mut lighting = vec![ColorRgbExp32 {
            r: 1,
            g: 1,
            b: 1,
            exponent: 0,
        }];
        for block in 0..blocks {
            let value = (block + 1) as u8;
            lighting.extend(
                [ColorRgbExp32 {
                    r: value,
                    g: value,
                    b: value,
                    exponent: 0,
                }; 4],
            );
        }
        lumps.push((LUMP_LIGHTING_HDR, bytemuck::cast_slice(&lighting).to_vec()));
    }

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

    /// The collision lumps' strides, which nothing else checks.
    ///
    /// Two of these carry an explicit `_pad` field because `bytemuck::Pod`
    /// refuses a type with implicit padding — and because the padding is real:
    /// the compiler that wrote the file put it there. A wrong stride reads a
    /// whole lump shifted, which produces a plausible tree of nonsense rather
    /// than an error.
    #[test]
    fn collision_lump_strides_match_the_file() {
        use std::mem::size_of;
        assert_eq!(size_of::<Plane>(), 20, "dplane_t");
        assert_eq!(size_of::<Node>(), 32, "dnode_t");
        assert_eq!(size_of::<Leaf>(), 32, "dleaf_t version 1");
        assert_eq!(size_of::<Brush>(), 12, "dbrush_t");
        assert_eq!(size_of::<BrushSide>(), 8, "dbrushside_t");
    }

    /// The `area:9`/`flags:7` bitfield, LSB first.
    #[test]
    fn leaf_area_and_flags_unpack() {
        let leaf = Leaf {
            contents: 0,
            cluster: 0,
            // flags = 3, area = 5
            area_flags: (3 << 9) | 5,
            mins: [0; 3],
            maxs: [0; 3],
            first_leaf_face: 0,
            num_leaf_faces: 0,
            first_leaf_brush: 0,
            num_leaf_brushes: 0,
            leaf_water_data_id: -1,
            _pad: 0,
        };
        assert_eq!(leaf.area(), 5);
        assert_eq!(leaf.flags(), 3);
    }
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
