//! Hand-built collision models for tests.
//!
//! Shared with `src/client/`'s movement tests, which need a room to walk in and
//! have no more business loading a `.bsp` than these do. Everything goes
//! through [`CollisionBsp::build`] rather than filling the private fields, so
//! the box-brush extraction and the surface table are under test too — a
//! fixture that skipped them would be testing a different program.

use glam::Vec3;

use super::{CollisionBsp, Contents};
use crate::engine::world::bsp::{
    Brush, BrushSide, Bsp, DispInfo, DispTri, DispVert, Edge, Face, Leaf, Model, Node, Plane,
    TexData, TexInfo,
};

/// Builds a collision model without a map.
///
/// Goes through [`CollisionBsp::build`] rather than filling the private
/// fields, so the box-brush extraction and the surface table are under
/// test too — a fixture that skipped them would be testing a different
/// program.
#[derive(Default)]
pub(crate) struct Fixture {
    pub(crate) planes: Vec<Plane>,
    pub(crate) brushes: Vec<Brush>,
    pub(crate) brush_sides: Vec<BrushSide>,
    pub(crate) leaves: Vec<Leaf>,
    pub(crate) nodes: Vec<Node>,
    pub(crate) leaf_brushes: Vec<u16>,
    pub(crate) models: Vec<Model>,
    pub(crate) vertices: Vec<[f32; 3]>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) surfedges: Vec<i32>,
    pub(crate) faces: Vec<Face>,
    pub(crate) disp_info: Vec<DispInfo>,
    pub(crate) disp_verts: Vec<DispVert>,
    pub(crate) disp_tris: Vec<DispTri>,
}

impl Fixture {
    pub(crate) fn plane(&mut self, normal: [f32; 3], dist: f32, axial: bool) -> u16 {
        // `plane_type` 0-2 names an axis, 3-5 means "not axial". Valve
        // calls it "trivial to regenerate", which is exactly what makes it
        // usable here: writing 3 for an axial plane keeps a box off the
        // box-brush path without changing its geometry, so the two paths
        // can be pointed at the same brush.
        let plane_type = match axial {
            true => normal.iter().position(|c| c.abs() == 1.0).unwrap_or(3) as i32,
            false => 3,
        };
        self.planes.push(Plane {
            normal,
            dist,
            plane_type,
        });
        self.planes.len() as u16 - 1
    }

    /// An axis-aligned box brush. `axial` false forces the plane path.
    pub(crate) fn add_box(
        &mut self,
        mins: Vec3,
        maxs: Vec3,
        contents: Contents,
        axial: bool,
    ) -> u16 {
        let first_side = self.brush_sides.len() as i32;
        for axis in 0..3 {
            for (sign, dist) in [(-1.0f32, -mins[axis]), (1.0, maxs[axis])] {
                let mut normal = [0.0; 3];
                normal[axis] = sign;
                let plane_num = self.plane(normal, dist, axial);
                self.brush_sides.push(BrushSide {
                    plane_num,
                    tex_info: -1,
                    disp_info: -1,
                    bevel: 0,
                    thin: 0,
                });
            }
        }
        self.brushes.push(Brush {
            first_side,
            num_sides: 6,
            contents: contents.0 as i32,
        });
        self.brushes.len() as u16 - 1
    }

    /// A displacement over a four-cornered face, with a per-vertex offset.
    ///
    /// `corners` are given in the order the `.bsp` would hold them —
    /// `p0 → p1 → p2 → p3` round the quad — and `start` is
    /// `ddispinfo_t::startPosition`, the corner the grid is rotated to begin
    /// at. `height(i, j)` is the displacement along `up` for grid position
    /// `(i, j)`, `i` running `p0 → p1` and `j` running `p0 → p3`, so a flat
    /// patch is `|_, _| 0.0`.
    ///
    /// **The winding decides which way the terrain is solid.** The triangle
    /// normals come out along `(p3 - p0) × (p1 - p0)`, and every test in
    /// `disp` is one-sided against that, so a quad wound the other way is
    /// terrain you fall through.
    pub(crate) fn add_displacement(
        &mut self,
        corners: [Vec3; 4],
        start: Vec3,
        power: i32,
        contents: Contents,
        flags: u32,
        height: impl Fn(usize, usize) -> f32,
    ) -> usize {
        let index = self.disp_info.len();
        let spacing = (1usize << power) + 1;
        let up = (corners[3] - corners[0])
            .cross(corners[1] - corners[0])
            .normalize();

        self.disp_info.push(DispInfo {
            start_position: start.to_array(),
            disp_vert_start: self.disp_verts.len() as i32,
            disp_tri_start: self.disp_tris.len() as i32,
            power,
            // The top bit is what makes the rest read as flags at all.
            min_tess: (0x8000_0000u32 | flags) as i32,
            smoothing_angle: 0.0,
            contents: contents.0 as i32,
            map_face: self.faces.len() as u16,
            _pad: 0,
            lightmap_alpha_start: -1,
            lightmap_sample_position_start: -1,
            _neighbors: [0xFF; 88],
            _allowed_verts: [0xFFFF_FFFF; 10],
        });

        // The grid is indexed `i * spacing + j`, and the fixture's `height` is
        // asked in the same order, so a test can reason about one corner.
        for i in 0..spacing {
            for j in 0..spacing {
                self.disp_verts.push(DispVert {
                    vector: up.to_array(),
                    dist: height(i, j),
                    alpha: 0.0,
                });
            }
        }
        for _ in 0..DispInfo::tri_count(power) {
            self.disp_tris.push(DispTri { tags: 0 });
        }

        self.add_face(corners, index as i16);
        index
    }

    /// A four-cornered face, built out of fresh vertices, edges and surfedges.
    fn add_face(&mut self, corners: [Vec3; 4], disp_info: i16) {
        let first_vertex = self.vertices.len() as u16;
        self.vertices.extend(corners.map(|v| v.to_array()));
        // Edge 0 is never used — a negative surfedge means "this edge,
        // backwards", and zero has no sign — so the first fixture to add a
        // face pads it out.
        if self.edges.is_empty() {
            self.edges.push(Edge { v: [0, 0] });
        }
        let first_edge = self.surfedges.len() as i32;
        for i in 0..4u16 {
            self.edges.push(Edge {
                v: [first_vertex + i, first_vertex + (i + 1) % 4],
            });
            self.surfedges.push(self.edges.len() as i32 - 1);
        }
        self.faces.push(Face {
            plane_num: 0,
            side: 0,
            on_node: 1,
            first_edge,
            num_edges: 4,
            tex_info: 0,
            disp_info,
            surface_fog_volume_id: -1,
            styles: [255; 4],
            light_ofs: -1,
            area: 0.0,
            lightmap_mins: [0, 0],
            lightmap_size: [0, 0],
            orig_face: -1,
            num_prims: 0,
            first_prim_id: 0,
            smoothing_groups: 0,
        });
    }

    /// One leaf holding every brush, under a node whose children are both
    /// that leaf.
    ///
    /// A degenerate tree on purpose: `CM_ClipBoxToBrush` always clips
    /// against the *whole* segment rather than the piece the recursion is
    /// looking at, so a single-leaf tree gives the same answers a real one
    /// does and isolates the brush maths. [`split`](Fixture::split) is the
    /// fixture that exercises the descent.
    pub(crate) fn single_leaf(self) -> CollisionBsp {
        // Zero, not the OR of the brush list: this leaf stands for the
        // open air the brushes sit in, and a leaf's contents describe its
        // own volume rather than everything touching it. See
        // [`CLeaf::contents`] — getting this backwards is what makes a
        // position test in mid-air report `all_solid`.
        self.single_leaf_with(Contents::EMPTY)
    }

    /// The same, for a leaf that is itself inside something — which is
    /// what a leaf in the middle of a water volume looks like, and what
    /// `all_contents` is summed from.
    pub(crate) fn single_leaf_with(mut self, leaf_contents: Contents) -> CollisionBsp {
        let all: Vec<u16> = (0..self.brushes.len() as u16).collect();
        self.leaf_brushes = all;
        self.leaves.push(Leaf {
            contents: leaf_contents.0 as i32,
            cluster: 0,
            area_flags: 0,
            mins: [-32768; 3],
            maxs: [32767; 3],
            first_leaf_face: 0,
            num_leaf_faces: 0,
            first_leaf_brush: 0,
            num_leaf_brushes: self.leaf_brushes.len() as u16,
            leaf_water_data_id: -1,
            _pad: 0,
        });
        // Both children are the one leaf, so every descent reaches it.
        let plane_num = self.plane([1.0, 0.0, 0.0], 0.0, true);
        self.nodes.push(Node {
            plane_num: plane_num as i32,
            children: [-1, -1],
            mins: [-32768; 3],
            maxs: [32767; 3],
            first_face: 0,
            num_faces: 0,
            area: -1,
            _pad: 0,
        });
        self.finish()
    }

    /// Two leaves either side of `x = 0`, so the trace has a real tree to
    /// descend and a real split to make.
    pub(crate) fn split(mut self, front: &[u16], back: &[u16]) -> CollisionBsp {
        let leaf = |brushes: &[u16], fixture: &mut Fixture| {
            let first = fixture.leaf_brushes.len() as u16;
            fixture.leaf_brushes.extend_from_slice(brushes);
            fixture.leaves.push(Leaf {
                contents: 0,
                cluster: 0,
                area_flags: 0,
                mins: [-32768; 3],
                maxs: [32767; 3],
                first_leaf_face: 0,
                num_leaf_faces: 0,
                first_leaf_brush: first,
                num_leaf_brushes: brushes.len() as u16,
                leaf_water_data_id: -1,
                _pad: 0,
            });
            fixture.leaves.len() as i32 - 1
        };
        let front_leaf = leaf(front, &mut self);
        let back_leaf = leaf(back, &mut self);

        let plane_num = self.plane([1.0, 0.0, 0.0], 0.0, true);
        self.nodes.push(Node {
            plane_num: plane_num as i32,
            // Child 0 is in front of the plane (x > 0).
            children: [-1 - front_leaf, -1 - back_leaf],
            mins: [-32768; 3],
            maxs: [32767; 3],
            first_face: 0,
            num_faces: 0,
            area: -1,
            _pad: 0,
        });
        self.finish()
    }

    /// A world subtree and a brush model's, side by side in one tree —
    /// which is what a real `.bsp` is. The world is model 0 and the brush
    /// model is model 1, exactly as an entity's `"model" "*1"` names it.
    ///
    /// The point of the shape is that **the two subtrees hold different
    /// brushes under different head nodes**: a `trace_model` that descended
    /// from node 0 instead of the model's own would find the world's brushes
    /// and pass every test that only checked distances.
    pub(crate) fn world_and_model(mut self, world: &[u16], model: &[u16]) -> CollisionBsp {
        let subtree = |brushes: &[u16], fixture: &mut Fixture| {
            let first = fixture.leaf_brushes.len() as u16;
            fixture.leaf_brushes.extend_from_slice(brushes);
            fixture.leaves.push(Leaf {
                // Open air, as in `single_leaf` — a leaf's contents are its
                // own volume's, not the OR of everything touching it.
                contents: 0,
                cluster: 0,
                area_flags: 0,
                mins: [-32768; 3],
                maxs: [32767; 3],
                first_leaf_face: 0,
                num_leaf_faces: 0,
                first_leaf_brush: first,
                num_leaf_brushes: brushes.len() as u16,
                leaf_water_data_id: -1,
                _pad: 0,
            });
            let leaf = fixture.leaves.len() as i32 - 1;

            // Both children are the one leaf, so every descent reaches it.
            let plane_num = fixture.plane([1.0, 0.0, 0.0], 0.0, true);
            fixture.nodes.push(Node {
                plane_num: plane_num as i32,
                children: [-1 - leaf, -1 - leaf],
                mins: [-32768; 3],
                maxs: [32767; 3],
                first_face: 0,
                num_faces: 0,
                area: -1,
                _pad: 0,
            });
            fixture.nodes.len() as i32 - 1
        };

        let world_head = subtree(world, &mut self);
        let model_head = subtree(model, &mut self);
        assert_eq!(world_head, 0, "the world has to be the first head node");

        for head_node in [world_head, model_head] {
            self.models.push(Model {
                mins: [-32768.0; 3],
                maxs: [32767.0; 3],
                // Not a render transform — a brush model's placement comes
                // from the entity that names it, never from here.
                origin: [0.0; 3],
                head_node,
                first_face: 0,
                num_faces: 0,
            });
        }
        self.finish()
    }

    pub(crate) fn finish(mut self) -> CollisionBsp {
        // The displacement-to-leaf lists are pushed down model 0's subtree, so
        // a fixture that never named a model still needs one. Node 0 is the
        // root in every shape this builds.
        if self.models.is_empty() {
            self.models.push(Model {
                mins: [-32768.0; 3],
                maxs: [32767.0; 3],
                origin: [0.0; 3],
                head_node: 0,
                first_face: 0,
                num_faces: 0,
            });
        }
        // One texinfo and texdata, so a displacement's surface resolves to a
        // real table entry rather than the null surface.
        let (texinfo, texdata, texdata_string_table) = match self.faces.is_empty() {
            true => (Vec::new(), Vec::new(), Vec::new()),
            false => (
                vec![TexInfo {
                    texture_vecs: [[0.0; 4]; 2],
                    lightmap_vecs: [[0.0; 4]; 2],
                    flags: 0,
                    tex_data: 0,
                }],
                vec![TexData {
                    reflectivity: [0.5; 3],
                    name_string_table_id: 0,
                    width: 64,
                    height: 64,
                    view_width: 64,
                    view_height: 64,
                }],
                vec!["nature/test_displacement".to_owned()],
            ),
        };

        let bsp = Bsp {
            game_lumps: Vec::new(),
            leaf_ambient: Vec::new(),
            leaf_ambient_index: Vec::new(),
            pak: std::sync::Arc::from(&[][..]),
            path: "test".to_owned(),
            version: 21,
            revision: 0,
            entity_lump: String::new(),
            vertices: self.vertices,
            edges: self.edges,
            surfedges: self.surfedges,
            faces: self.faces,
            texinfo,
            texdata,
            texdata_string_table,
            models: self.models,
            lighting: Vec::new(),
            lighting_is_hdr: false,
            level_flags: 0,
            planes: self.planes,
            nodes: self.nodes,
            leaves: self.leaves,
            leaf_brushes: self.leaf_brushes,
            brushes: self.brushes,
            brush_sides: self.brush_sides,
            disp_info: self.disp_info,
            disp_verts: self.disp_verts,
            disp_tris: self.disp_tris,
        };
        CollisionBsp::build(&bsp)
    }
}
