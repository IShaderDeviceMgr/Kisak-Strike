//! Hand-built collision models for tests.
//!
//! Shared with `src/client/`'s movement tests, which need a room to walk in and
//! have no more business loading a `.bsp` than these do. Everything goes
//! through [`CollisionBsp::build`] rather than filling the private fields, so
//! the box-brush extraction and the surface table are under test too — a
//! fixture that skipped them would be testing a different program.

use glam::Vec3;

use super::carve::{LivePortal, PortalHole, PortalLink};
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
    /// `SURF_*` for the one texinfo [`bsp`](Fixture::bsp) emits, which is what
    /// a brush side built by [`add_surfaced_box`](Fixture::add_surfaced_box)
    /// names.
    pub(crate) surface_flags: i32,
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

    /// A box brush whose sides name a real surface, so that a trace hitting it
    /// comes back with `flags` in [`Trace::surface_flags`].
    ///
    /// One caller: the light cache's skylight test, which decides whether a
    /// point is in sunlight by asking whether a ray fired at the sky hit
    /// `SURF_SKY`. There is one texinfo in a [`Fixture`], so the flags are the
    /// fixture's rather than the box's.
    ///
    /// [`Trace::surface_flags`]: super::Trace::surface_flags
    pub(crate) fn add_surfaced_box(
        &mut self,
        mins: Vec3,
        maxs: Vec3,
        contents: Contents,
        flags: i32,
    ) -> u16 {
        self.surface_flags = flags;
        let brush = self.add_box(mins, maxs, contents, true);
        let sides = &self.brushes[brush as usize];
        let first = sides.first_side as usize;
        let count = sides.num_sides as usize;
        for side in &mut self.brush_sides[first..first + count] {
            side.tex_info = 0;
        }
        brush
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
            allowed_verts: [0xFFFF_FFFF; 10],
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

    pub(crate) fn finish(self) -> CollisionBsp {
        CollisionBsp::build(&self.bsp())
    }

    /// The same fixture as a `.bsp`, for the readers that want one.
    ///
    /// `world/disp/` builds render geometry out of a `Bsp` rather than a
    /// [`CollisionBsp`], and its tests need the same hand-built patches these
    /// do — so the two stay one construction site, and a fixture change cannot
    /// make the drawn surface and the solid one describe different geometry.
    pub(crate) fn bsp(mut self) -> Bsp {
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
        let (texinfo, texdata, texdata_string_table) =
            match self.faces.is_empty() && self.surface_flags == 0 {
                true => (Vec::new(), Vec::new(), Vec::new()),
                false => (
                    vec![TexInfo {
                        texture_vecs: [[0.0; 4]; 2],
                        lightmap_vecs: [[0.0; 4]; 2],
                        flags: self.surface_flags,
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

        Bsp {
            game_lumps: Vec::new(),
            leaf_ambient: Vec::new(),
            leaf_ambient_index: Vec::new(),
            world_lights: Vec::new(),
            world_lights_are_hdr: true,
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
            leaf_faces: Vec::new(),
            visibility: Vec::new(),
            areas: Vec::new(),
            area_portals: Vec::new(),
            clip_portal_verts: Vec::new(),
            leaf_brushes: self.leaf_brushes,
            brushes: self.brushes,
            brush_sides: self.brush_sides,
            disp_info: self.disp_info,
            disp_verts: self.disp_verts,
            disp_tris: self.disp_tris,
        }
    }
}

// ---------------------------------------------------------------------------
// Two rooms and a linked pair
// ---------------------------------------------------------------------------

/// Where the floor of both rooms is, so that a test can stand a player on it:
/// the top of the slab, and also exactly the bottom edge of both portals.
///
/// A portal's centre is at `z = 0` and its half-height is 56, so a player
/// standing here walks straight into the hole with no step up.
pub(crate) const PORTAL_ROOM_FLOOR: f32 = -56.0;

/// How thick both rooms' walls are.
///
/// Thick enough that a player hull standing in the middle of the hole is out
/// of reach of *this* room's floor even after the box sweep's plane expansion
/// — 16 units of hull plus room to spare. A thin slab would leave the near
/// floor catching the player at exactly the height the far floor does, and a
/// test that cannot tell the two apart proves nothing.
pub(crate) const PORTAL_ROOM_WALL: f32 = 40.0;

/// Two rooms with a linked portal pair between them — the fixture every
/// stage-4 test walks through.
///
/// Shared between `trace::carve`'s tests and `client::movement`'s because the
/// two halves of the teleport are only meaningful against the same geometry:
/// one asserts that the far room's floor holds the player up, the other that
/// they come out standing on it.
pub(crate) struct PortalRooms {
    pub(crate) collision: CollisionBsp,
    /// On a wall through `x = 0`, facing `+X`, with the room in front of it.
    pub(crate) blue: LivePortal,
    /// A thousand units away on a wall through `y = 0`, facing `+Y`.
    ///
    /// **A different yaw on purpose.** With both portals facing the same way
    /// the pair's matrix rotates vectors by nothing at all and every sign
    /// error in the teleport passes.
    pub(crate) orange: LivePortal,
}

impl PortalRooms {
    pub(crate) const BLUE_ID: u64 = 1;
    pub(crate) const ORANGE_ID: u64 = 2;

    /// Both portals, as [`PortalHoles::sync`](super::PortalHoles::sync) wants
    /// them.
    pub(crate) fn live(&self) -> [LivePortal; 2] {
        [self.blue, self.orange]
    }

    /// The same, with neither portal knowing about the other — the control for
    /// every test about the far side.
    pub(crate) fn unlinked(&self) -> [LivePortal; 2] {
        [
            LivePortal {
                link: None,
                ..self.blue
            },
            LivePortal {
                link: None,
                ..self.orange
            },
        ]
    }
}

/// Builds [`PortalRooms`].
///
/// Each room is a wall with the portal on its face, a floor 56 units below the
/// portal's centre, and nothing else. The two are far enough apart that
/// neither one's carve can see the other's geometry, which is what makes a
/// trace that finds the far room's floor proof that it went through the
/// matrix.
pub(crate) fn portal_rooms() -> PortalRooms {
    portal_rooms_with(&[])
}

/// The same, plus `extra` boxes — whatever the test wants to put in one of the
/// two rooms.
///
/// Every stage-4 test is about the geometry at the *far* end being taken into
/// account, so every one of them needs something at the far end that the near
/// end does not have. Passing it in keeps the two rooms themselves identical
/// between tests.
pub(crate) fn portal_rooms_with(extra: &[(Vec3, Vec3)]) -> PortalRooms {
    // `teleport_matrix` rather than a second derivation: there is **one**
    // teleport matrix in this port, the server computes it, and a fixture that
    // spelled it again could agree with a wrong carve. `fixture` is
    // `#[cfg(test)]`, so naming `server/` from `trace/` here costs the port no
    // layering.
    use crate::server::classes::portal::teleport_matrix;

    let blue_at = (Vec3::ZERO, Vec3::ZERO);
    let orange_at = (Vec3::new(1000.0, 0.0, 0.0), Vec3::new(0.0, 90.0, 0.0));
    let hole = |(origin, angles): (Vec3, Vec3)| PortalHole::new(origin, angles, 32.0, 56.0);

    let mut fixture = Fixture::default();
    let solid = |fixture: &mut Fixture, mins: Vec3, maxs: Vec3| {
        fixture.add_box(mins, maxs, Contents::SOLID, true);
    };
    // Blue's room: the wall it is on, and the floor in front of it. The wall
    // is [`PORTAL_ROOM_WALL`] thick rather than a slab, so that the hole
    // through it is a *tunnel* a hull can be wholly inside — which is the only
    // place the far room's floor is the one thing holding the player up.
    solid(
        &mut fixture,
        Vec3::new(-PORTAL_ROOM_WALL, -256.0, -256.0),
        Vec3::new(0.0, 256.0, 256.0),
    );
    solid(
        &mut fixture,
        Vec3::new(0.0, -200.0, PORTAL_ROOM_FLOOR - 20.0),
        Vec3::new(200.0, 200.0, PORTAL_ROOM_FLOOR),
    );
    // Orange's, on a wall the other way round.
    solid(
        &mut fixture,
        Vec3::new(872.0, -PORTAL_ROOM_WALL, -256.0),
        Vec3::new(1128.0, 0.0, 256.0),
    );
    solid(
        &mut fixture,
        Vec3::new(900.0, 0.0, PORTAL_ROOM_FLOOR - 20.0),
        Vec3::new(1100.0, 200.0, PORTAL_ROOM_FLOOR),
    );

    for &(mins, maxs) in extra {
        solid(&mut fixture, mins, maxs);
    }

    PortalRooms {
        collision: fixture.single_leaf(),
        blue: LivePortal {
            id: PortalRooms::BLUE_ID,
            hole: hole(blue_at),
            link: Some(PortalLink {
                exit_id: PortalRooms::ORANGE_ID,
                exit: hole(orange_at),
                to_exit: teleport_matrix(blue_at, orange_at),
                to_entrance: teleport_matrix(orange_at, blue_at),
            }),
        },
        orange: LivePortal {
            id: PortalRooms::ORANGE_ID,
            hole: hole(orange_at),
            link: Some(PortalLink {
                exit_id: PortalRooms::BLUE_ID,
                exit: hole(blue_at),
                to_exit: teleport_matrix(orange_at, blue_at),
                to_entrance: teleport_matrix(blue_at, orange_at),
            }),
        },
    }
}
