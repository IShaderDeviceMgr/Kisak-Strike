//! Hand-built collision models for tests.
//!
//! Shared with `src/client/`'s movement tests, which need a room to walk in and
//! have no more business loading a `.bsp` than these do. Everything goes
//! through [`CollisionBsp::build`] rather than filling the private fields, so
//! the box-brush extraction and the surface table are under test too — a
//! fixture that skipped them would be testing a different program.

use glam::Vec3;

use super::{CollisionBsp, Contents};
use crate::engine::world::bsp::{Brush, BrushSide, Bsp, Leaf, Node, Plane};

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
    pub(crate) fn add_box(&mut self, mins: Vec3, maxs: Vec3, contents: Contents, axial: bool) -> u16 {
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
        let mut leaf = |brushes: &[u16], fixture: &mut Fixture| {
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

    pub(crate) fn finish(self) -> CollisionBsp {
        let bsp = Bsp {
            path: "test".to_owned(),
            version: 21,
            revision: 0,
            entity_lump: String::new(),
            vertices: Vec::new(),
            edges: Vec::new(),
            surfedges: Vec::new(),
            faces: Vec::new(),
            texinfo: Vec::new(),
            texdata: Vec::new(),
            texdata_string_table: Vec::new(),
            models: Vec::new(),
            lighting: Vec::new(),
            lighting_is_hdr: false,
            level_flags: 0,
            planes: self.planes,
            nodes: self.nodes,
            leaves: self.leaves,
            leaf_brushes: self.leaf_brushes,
            brushes: self.brushes,
            brush_sides: self.brush_sides,
        };
        CollisionBsp::build(&bsp)
    }
}
