//! The physics environment: `IPhysicsEnvironment`, on Rapier.
//!
//! `legacy/vphysics/physics_environment.cpp` (2,335 lines) and
//! `physics_object.cpp` (2,060) reduced to the part that is *knowledge* — the
//! units, the timestep, the gravity, and how a `.phy`'s numbers become a rigid
//! body. Everything else those files contain is IVP's encoding of a solver
//! Rapier already has.
//!
//! # Units: this port does not convert, and Valve wished it did not
//!
//! Valve scales every length by `METERS_PER_INCH` at every boundary, because
//! IVP is tuned for metres and offered no alternative. `convert.h`'s own first
//! line is `// UNDONE: Remove all conversion/scaling`.
//!
//! Rapier offers the alternative: `IntegrationParameters::length_unit` is
//! "how many of your units equal one metre", and every internal tolerance is
//! scaled by it. So this environment runs in **Source units** with
//! `length_unit = 39.3701`, gravity is `600` rather than `15.24`, and the only
//! place a metre appears in the whole module is
//! [`collide::ivp_to_source`](super::collide::ivp_to_source), which has to
//! swap the axes anyway.
//!
//! Mass stays in kilograms, exactly as Valve's does — a Source force is
//! therefore kg·units/s², which is self-consistent and is what the shipped
//! game computes in too.
//!
//! # The timestep
//!
//! `physics.cpp:265` pins the physics step at **1/64 s regardless of the
//! game's tick rate**, and the comment says why: smaller steps were never
//! tested and made guns bounce. This port's server tick is already 64 Hz, so
//! the two coincide and [`Environment::step`] is called exactly once per
//! server tick with no accumulator of its own.
//!
//! # What is deliberately missing
//!
//! `portdocs/VPHYSICS.md` §9 has the list and the reasons. The one that will
//! be noticed first: **there is no shadow controller**, so the player walks
//! through a cube rather than pushing it. A *moving brush* is a kinematic
//! body here, which is the half of the shadow controller a door actually uses.


use glam::{Mat3, Quat, Vec3};
use rapier3d::dynamics::{MassProperties, RigidBodyBuilder, RigidBodyHandle, RigidBodyType};
use rapier3d::geometry::{ColliderBuilder, SharedShape};
use rapier3d::math::Pose;
use rapier3d::pipeline::PhysicsWorld;
use rapier3d::prelude::CoefficientCombineRule;

use super::collide::{Solid, SolidParams, UNITS_PER_METER};
use super::surfaceprops::{SurfaceProps, Surface};
use crate::math::{angle_matrix, matrix_angles};

/// `sv_gravity` for Portal 2, in Source units per second squared.
///
/// The same number as [`crate::client::movement::SV_GRAVITY`], named again
/// here rather than imported because this module must not depend on `client/`
/// — and because the *reason* differs: `physics.cpp:279` reads the convar into
/// the environment once, at `LevelInitPreEntity`, so a mid-level change to
/// `sv_gravity` does not reach vphysics in the shipped game either.
pub const GRAVITY: f32 = 600.0;

/// `physics.cpp:274` — "always run 64 tick physics".
pub const TIMESTEP: f32 = 1.0 / 64.0;

/// `objectparams_t::rotInertiaLimit` from `g_PhysDefaultObjectParams`
/// (`physics_shared.cpp:49`), applied by IVP as `auto_check_rot_inertia`.
///
/// Every principal moment below 5% of the inertia vector's length is raised to
/// it, which stops a flat or needle-shaped hull spinning about its thin axis
/// arbitrarily fast.
const ROT_INERTIA_LIMIT: f32 = 0.05;

/// `VPHYSICS_MIN_MASS` / `VPHYSICS_MAX_MASS`
/// (`legacy/public/vphysics_interface.h`), in kilograms.
const MIN_MASS: f32 = 0.1;
const MAX_MASS: f32 = 50_000.0;

/// A handle to a body in the [`Environment`].
///
/// `IPhysicsObject *` with the pointer taken out. Generational, so a handle to
/// a removed body resolves to `None` rather than to whatever was created next
/// — which matters because the entity that owns one can be removed by an input
/// in the same tick the environment is stepped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BodyId {
    slot: u32,
    generation: u32,
}

impl BodyId {
    /// Packs into the `u128` Rapier carries on a body for us, so that the
    /// active-body sweep can name its owner without a second map.
    fn to_user_data(self) -> u128 {
        (self.slot as u128) << 32 | self.generation as u128
    }

    fn from_user_data(data: u128) -> BodyId {
        BodyId {
            slot: (data >> 32) as u32,
            generation: data as u32,
        }
    }
}

/// How a body is driven. `VPhysicsInitStatic` / `VPhysicsInitShadow` /
/// `VPhysicsInitNormal`, in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Never moves: the world, a displacement, a static prop, a brush entity
    /// that is not a mover.
    Static,
    /// Moved by the game, not by the solver — a door. Valve's shadow object
    /// with `allowPhysicsMovement` and `allowPhysicsRotation` both false, which
    /// is precisely a kinematic body whose pose the game writes each tick.
    Kinematic,
    /// Moved by the solver. A physics prop. The cube.
    Dynamic,
}

/// A collision model converted to Rapier shapes **once**, ready to be placed
/// any number of times.
///
/// `CPhysCollide`. A model placed fifty times hulls its ledges once: a
/// [`SharedShape`] is reference-counted, so a placement clones a pointer.
#[derive(Debug, Clone, Default)]
pub struct Hulls {
    shapes: Vec<SharedShape>,
    /// Ledges that could not be turned into a convex hull — degenerate ones,
    /// with fewer than four non-coplanar points.
    ///
    /// Counted rather than refused, because a solid is a *set* of pieces and
    /// losing one of eighty is better than losing the model. The depot test
    /// asserts the total over the shipped game.
    pub degenerate: usize,
}

impl Hulls {
    /// Every ledge of one [`Solid`], as a convex hull each.
    ///
    /// > **One collider per ledge, not one compound.** A ledge is a
    /// > brush-shaped convex piece and Rapier's broad phase would rather have
    /// > eighty small AABBs than one that covers the model. It is also what
    /// > lets a ledge carry its own surface property, which is the shape
    /// > `IVP_Compact_Triangle::material_index` wants even though
    /// > `portdocs/VPHYSICS.md` §3.5 explains why the world cannot use it.
    ///
    /// Valve builds the hull from the ledge's *triangles*, which are already a
    /// closed convex surface; this recomputes it from the points with
    /// quickhull instead. That is a deliberate swap of a little load time for
    /// not having to trust a winding convention — and the points come from the
    /// triangles either way, so nothing is thrown away. See
    /// `rustdocs/VPHYSICS.md`.
    pub fn from_solid(solid: &Solid) -> Hulls {
        let mut out = Hulls::default();
        for ledge in &solid.ledges {
            match ColliderBuilder::convex_hull(&ledge.points) {
                Some(builder) => out.shapes.push(builder.build().shared_shape().clone()),
                None => out.degenerate += 1,
            }
        }
        out
    }

    /// A triangle mesh — what a displacement is.
    ///
    /// `portdocs/VPHYSICS.md` §2.5: displacement collision is *not* in
    /// `LUMP_PHYSCOLLIDE`, every shipped map says `virtualterrain {}`, and this
    /// port already has the grid. 643 lines of `physics_virtualmesh.cpp`
    /// deleted for one constructor.
    pub fn from_mesh(points: Vec<Vec3>, indices: Vec<[u32; 3]>) -> Hulls {
        let mut out = Hulls::default();
        if points.len() < 3 || indices.is_empty() {
            return out;
        }
        match ColliderBuilder::trimesh(points, indices) {
            Ok(builder) => out.shapes.push(builder.build().shared_shape().clone()),
            Err(_) => out.degenerate += 1,
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.shapes.len()
    }
}

/// What a dynamic body needs that a static one does not: the `.phy`'s mass,
/// its mass centre and its rotational inertia.
///
/// Built by [`Mass::from_solid`], which is where
/// `portdocs/VPHYSICS.md` §3.3's arithmetic lives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mass {
    pub mass: f32,
    pub center: Vec3,
    /// The principal moments, in kg·units².
    pub inertia: Vec3,
    pub damping: f32,
    pub rot_damping: f32,
}

impl Mass {
    /// `IVP_Real_Object::init_object_core` (`ivp_object.cxx:845`) plus
    /// `CPhysicsObject`'s template fill (`physics_object.cpp:1480`).
    ///
    /// ```text
    /// rot_inertia = surface.rotation_inertia * solid.inertia * mass
    /// ```
    ///
    /// then every component below `ROT_INERTIA_LIMIT` of the vector's length
    /// raised to it.
    ///
    /// > **`Solid::rotation_inertia` is not a moment of inertia and this
    /// > reproduces that.** `IVP_Rot_Inertia_Solver`
    /// > (`ivp_rot_inertia_solver.cxx:210`) integrates the second moments
    /// > `⟨x²⟩`, `⟨y²⟩`, `⟨z²⟩` and then writes `sqrt(⟨y²⟩² + ⟨z²⟩²)` where
    /// > the moment about x is `⟨y²⟩ + ⟨z²⟩`. For a cube of side *a* the two
    /// > differ by exactly √2/2: `a²/6` against `(a²/12)·√2`. Checked against
    /// > the shipped `models/props/metal_box.phy`, whose hull is 35.64 units
    /// > and whose file stores 0.09632 m² where the truth is 0.13655.
    /// >
    /// > Every weighted cube in Portal 2 therefore rotates **1.41× more
    /// > easily** than a real one, has done since 2004, and every throw and
    /// > drop in the game is tuned against it. Reproducing a bug is not
    /// > usually the right call; reproducing *this* one is, because the
    /// > alternative is a cube that behaves correctly and wrongly at the same
    /// > time. [`Mass::true_inertia`] is what the other answer would be.
    pub fn from_solid(solid: &Solid, params: &SolidParams) -> Mass {
        let mass = params.mass.clamp(MIN_MASS, MAX_MASS);
        let factor = if params.inertia <= 0.0 {
            1.0
        } else {
            params.inertia.min(1e18)
        };
        let mut inertia = solid.rotation_inertia * factor * mass;
        let floor = inertia.length() * ROT_INERTIA_LIMIT;
        inertia = inertia.max(Vec3::splat(floor));
        Mass {
            mass,
            center: solid.mass_center,
            inertia,
            damping: params.damping,
            rot_damping: params.rot_damping,
        }
    }

    /// What the principal moments would be if `Solid::rotation_inertia` meant
    /// what it looks like it means — i.e. undoing IVP's `sqrt(a² + b²)` back
    /// into `a + b`.
    ///
    /// Not used. It exists so that the claim in [`Mass::from_solid`] can be
    /// checked rather than believed, and so that the switch is one call away
    /// if this port ever decides Portal 2's physics need not match the shipped
    /// game's.
    #[allow(dead_code)]
    pub fn true_inertia(solid: &Solid, params: &SolidParams) -> Vec3 {
        // IVP stored s_i = sqrt(m_j² + m_k²) for the three cyclic pairs of the
        // second moments m. Squaring gives s_i² = m_j² + m_k², a linear system
        // in the squares whose solution is m_i² = (s_j² + s_k² − s_i²)/2.
        let s = solid.rotation_inertia * solid.rotation_inertia;
        let squares = Vec3::new(
            (s.y + s.z - s.x) * 0.5,
            (s.z + s.x - s.y) * 0.5,
            (s.x + s.y - s.z) * 0.5,
        )
        .max(Vec3::ZERO);
        let m = Vec3::new(squares.x.sqrt(), squares.y.sqrt(), squares.z.sqrt());
        let mass = params.mass.clamp(MIN_MASS, MAX_MASS);
        Vec3::new(m.y + m.z, m.z + m.x, m.x + m.y) * mass
    }
}

struct Body {
    handle: RigidBodyHandle,
    generation: u32,
}

/// The environment. `physenv`.
///
/// Level-scoped, exactly as Valve's is: `CPhysicsHook::LevelInitPreEntity`
/// creates it and `LevelShutdownPostEntity` destroys it.
pub struct Environment {
    world: PhysicsWorld,
    surfaces: SurfaceProps,
    bodies: Vec<Option<Body>>,
    free: Vec<u32>,
    generation: u32,
    /// Bodies removed since the last [`Environment::step`], so that a caller
    /// walking the active list cannot be handed one.
    live: usize,
}

impl Environment {
    /// A new, empty environment with Portal 2's gravity and timestep.
    pub fn new(surfaces: SurfaceProps) -> Environment {
        let mut world = PhysicsWorld::new();
        // Source's z is up, and gravity is negative along it.
        world.gravity = Vec3::new(0.0, 0.0, -GRAVITY);
        world.integration_parameters.dt = TIMESTEP;
        // The whole of the units decision, in one line. See the module docs.
        world.integration_parameters.length_unit = UNITS_PER_METER;
        Environment {
            world,
            surfaces,
            bodies: Vec::new(),
            free: Vec::new(),
            generation: 1,
            live: 0,
        }
    }

    #[allow(dead_code)]
    pub fn surfaces(&self) -> &SurfaceProps {
        &self.surfaces
    }

    /// How many bodies are in the environment.
    pub fn len(&self) -> usize {
        self.live
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// `CreatePolyObjectStatic` / `CreatePolyObject`.
    ///
    /// `mass` is `None` for [`Motion::Static`] and [`Motion::Kinematic`],
    /// which have neither — Valve's `physical_unmoveable` objects never reach
    /// the mass-properties branch at all.
    ///
    /// Returns `None` when the model has no usable shape, which is Valve's
    /// `PhysModelCreate` returning `NULL` for a `vcollide_t` with no solids.
    pub fn add(
        &mut self,
        motion: Motion,
        hulls: &Hulls,
        origin: Vec3,
        angles: Vec3,
        surface: &str,
        mass: Option<Mass>,
    ) -> Option<BodyId> {
        if hulls.is_empty() {
            return None;
        }
        let id = self.reserve();
        let pose = to_pose(origin, angles);
        let builder = match motion {
            Motion::Static => RigidBodyBuilder::fixed(),
            Motion::Kinematic => RigidBodyBuilder::kinematic_position_based(),
            Motion::Dynamic => RigidBodyBuilder::dynamic(),
        };
        let mut builder = builder.pose(pose).user_data(id.to_user_data());
        if let Some(mass) = mass {
            builder = builder
                .additional_mass_properties(MassProperties::new(mass.center, mass.mass, mass.inertia))
                .linear_damping(mass.damping)
                .angular_damping(mass.rot_damping);
        }
        let handle = self.world.insert_body(builder);

        let surface = self.surfaces.resolve(surface).clone();
        for shape in &hulls.shapes {
            let collider = collider(shape.clone(), &surface);
            self.world.insert_collider(collider, Some(handle));
        }
        self.bodies[id.slot as usize] = Some(Body {
            handle,
            generation: id.generation,
        });
        self.live += 1;
        Some(id)
    }

    /// `PhysDestroyObject`.
    pub fn remove(&mut self, id: BodyId) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        self.world.remove_body(handle);
        self.bodies[id.slot as usize] = None;
        self.free.push(id.slot);
        self.live -= 1;
    }

    /// Where a body is, as a Source origin and `QAngle`.
    ///
    /// `IPhysicsObject::GetPosition`. The per-tick writeback does **not** go
    /// through this — [`active`](Environment::active) is `GetActiveObjects`
    /// and hands out the pose with the handle — so this is for asking about
    /// one body in particular: a test, a console command, a class that wants
    /// to know where its own prop ended up.
    #[allow(dead_code)]
    pub fn pose(&self, id: BodyId) -> Option<(Vec3, Vec3)> {
        let handle = self.handle(id)?;
        let body = self.world.bodies.get(handle)?;
        Some(from_pose(body.position()))
    }

    /// `IPhysicsObject::UpdateShadow` — where the *game* says a kinematic body
    /// now is.
    ///
    /// Uses `set_next_kinematic_position` rather than `set_position`, which is
    /// the difference between a door that shoves what it meets and a door that
    /// teleports through it: Rapier derives the body's velocity for this step
    /// from the gap between its current pose and its next one, and a hard
    /// `set_position` leaves that velocity at zero.
    pub fn set_kinematic_pose(&mut self, id: BodyId, origin: Vec3, angles: Vec3) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        let pose = to_pose(origin, angles);
        if let Some(body) = self.world.bodies.get_mut(handle) {
            if body.body_type() == RigidBodyType::KinematicPositionBased {
                body.set_next_kinematic_position(pose);
            } else {
                body.set_position(pose, true);
            }
        }
    }

    /// `IPhysicsObject::EnableMotion`.
    ///
    /// Valve freezes the object in place while leaving it collidable, which is
    /// a fixed body here. Re-enabling restores [`Motion::Dynamic`] and wakes
    /// it, which is `EnableMotion( true )`'s own `Wake()`.
    pub fn enable_motion(&mut self, id: BodyId, enable: bool) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        let Some(body) = self.world.bodies.get_mut(handle) else {
            return;
        };
        let kind = if enable {
            RigidBodyType::Dynamic
        } else {
            RigidBodyType::Fixed
        };
        body.set_body_type(kind, enable);
    }

    /// Whether the solver is still integrating this body — `IsAsleep`,
    /// negated. Like [`pose`](Environment::pose), for asking about one body.
    #[allow(dead_code)]
    pub fn is_awake(&self, id: BodyId) -> bool {
        self.handle(id)
            .and_then(|h| self.world.bodies.get(h))
            .is_some_and(|b| !b.is_sleeping())
    }

    /// `IPhysicsObject::Wake`.
    pub fn wake(&mut self, id: BodyId) {
        if let Some(handle) = self.handle(id) {
            self.world.wake_up(handle, true);
        }
    }

    /// `IPhysicsObject::Sleep` — stop simulating until something touches it.
    pub fn sleep(&mut self, id: BodyId) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        if let Some(body) = self.world.bodies.get_mut(handle) {
            body.sleep();
        }
    }

    /// `IPhysicsObject::GetMass`, in kilograms. `None` for a static body,
    /// which Rapier reports as infinite mass and Valve reports as 0.
    pub fn mass(&self, id: BodyId) -> Option<f32> {
        let handle = self.handle(id)?;
        let mass = self.world.bodies.get(handle)?.mass();
        (mass.is_finite() && mass > 0.0).then_some(mass)
    }

    /// `IPhysicsObject::ApplyForceCenter` — a force at the mass centre, in
    /// kg·units/s², applied for one step.
    ///
    /// Rapier's `add_force` accumulates over the step and clears afterwards,
    /// which is IVP's `async_push_core` in every way that is observable.
    pub fn apply_force_center(&mut self, id: BodyId, force: Vec3) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        if let Some(body) = self.world.bodies.get_mut(handle) {
            body.add_force(force, true);
        }
    }

    /// `physenv->Simulate( TICK_INTERVAL )`. One fixed step; see the module
    /// docs on why there is no accumulator here.
    pub fn step(&mut self) {
        self.world.step();
    }

    /// `physenv->GetActiveObjects` — every body the solver moved, with where
    /// it moved to.
    ///
    /// This is `PhysFrame`'s second phase (`physics.cpp:1778`) and it is the
    /// only thing the writeback needs: a body that did not move does not need
    /// its entity touched, and a sleeping cube costs nothing.
    pub fn active(&self) -> impl Iterator<Item = (BodyId, Vec3, Vec3)> + '_ {
        self.world.active_bodies().filter_map(|(_, body)| {
            let id = BodyId::from_user_data(body.user_data);
            // A body whose slot has been reused would answer with a stale id;
            // it cannot happen between a removal and a step, because removal
            // takes the body out of the world too, but the check is free.
            let live = self
                .bodies
                .get(id.slot as usize)
                .and_then(|slot| slot.as_ref())
                .is_some_and(|b| b.generation == id.generation);
            if !live || body.body_type() != RigidBodyType::Dynamic {
                return None;
            }
            let (origin, angles) = from_pose(body.position());
            Some((id, origin, angles))
        })
    }

    fn handle(&self, id: BodyId) -> Option<RigidBodyHandle> {
        self.bodies
            .get(id.slot as usize)?
            .as_ref()
            .filter(|b| b.generation == id.generation)
            .map(|b| b.handle)
    }

    fn reserve(&mut self) -> BodyId {
        self.generation = self.generation.wrapping_add(1).max(1);
        match self.free.pop() {
            Some(slot) => BodyId {
                slot,
                generation: self.generation,
            },
            None => {
                self.bodies.push(None);
                BodyId {
                    slot: (self.bodies.len() - 1) as u32,
                    generation: self.generation,
                }
            }
        }
    }
}

/// One collider, with the friction and elasticity of a surface property.
///
/// The combine rules are IVP's: both coefficients **multiply** across the pair
/// (`ivp_material.cxx:13`). See
/// [`surfaceprops`](super::surfaceprops) on the clamp that cannot follow.
fn collider(shape: SharedShape, surface: &Surface) -> rapier3d::geometry::Collider {
    ColliderBuilder::new(shape)
        // The body carries the whole of its mass properties, taken from the
        // `.phy` — so the colliders must contribute none of their own.
        .density(0.0)
        .friction(surface.friction_coefficient())
        .friction_combine_rule(CoefficientCombineRule::Multiply)
        .restitution(surface.restitution())
        .restitution_combine_rule(CoefficientCombineRule::Multiply)
        .build()
}

/// A Source origin and `QAngle` as a Rapier pose.
fn to_pose(origin: Vec3, angles: Vec3) -> Pose {
    Pose::from_parts(origin, Quat::from_mat3(&angle_matrix(angles)))
}

/// The inverse, which is what `IPhysicsObject::GetPosition` gives
/// `VPhysicsUpdate`.
fn from_pose(pose: &Pose) -> (Vec3, Vec3) {
    (
        pose.translation,
        matrix_angles(Mat3::from_quat(pose.rotation)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vphysics::collide::{Ledge, Solid};

    /// A box hull of the given half-extents, as one ledge.
    fn cube_solid(half: f32) -> Solid {
        let mut points = Vec::new();
        for i in 0..8 {
            points.push(Vec3::new(
                if i & 1 == 0 { -half } else { half },
                if i & 2 == 0 { -half } else { half },
                if i & 4 == 0 { -half } else { half },
            ));
        }
        Solid {
            mass_center: Vec3::ZERO,
            // `(a²/12)·√2` per axis, which is what IVP stores for a cube — see
            // `Mass::from_solid`.
            rotation_inertia: Vec3::splat((2.0 * half).powi(2) / 12.0 * 2f32.sqrt()),
            radius: half * 3f32.sqrt(),
            ledges: vec![Ledge {
                points,
                triangles: Vec::new(),
                material: 0,
                mixed_materials: false,
            }],
        }
    }

    fn params(mass: f32) -> SolidParams {
        SolidParams {
            mass,
            ..Default::default()
        }
    }

    /// A big flat floor at z = 0, as a static body.
    fn floor(env: &mut Environment) -> BodyId {
        let mut points = Vec::new();
        for i in 0..8 {
            points.push(Vec3::new(
                if i & 1 == 0 { -4096.0 } else { 4096.0 },
                if i & 2 == 0 { -4096.0 } else { 4096.0 },
                if i & 4 == 0 { -64.0 } else { 0.0 },
            ));
        }
        let solid = Solid {
            mass_center: Vec3::ZERO,
            rotation_inertia: Vec3::ONE,
            radius: 4096.0,
            ledges: vec![Ledge {
                points,
                triangles: Vec::new(),
                material: 0,
                mixed_materials: false,
            }],
        };
        let hulls = Hulls::from_solid(&solid);
        env.add(Motion::Static, &hulls, Vec3::ZERO, Vec3::ZERO, "default", None)
            .expect("a floor")
    }

    /// The whole units decision, checked against a closed form rather than
    /// against Rapier.
    ///
    /// A body released in a vacuum falls `½gt²`. With `GRAVITY` in Source
    /// units and `length_unit` set to match, half a second is
    /// `0.5 × 600 × 0.25` = **75 units**, which is a foot and a half in
    /// Valve's scale and 1.9 m in the real one. If the port had kept Valve's
    /// metre conversion and forgotten a factor, this is the test that would
    /// say so.
    #[test]
    fn a_body_in_a_vacuum_falls_at_sv_gravity() {
        let mut env = Environment::new(SurfaceProps::default());
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        let body = env
            .add(
                Motion::Dynamic,
                &hulls,
                Vec3::new(0.0, 0.0, 1000.0),
                Vec3::ZERO,
                "default",
                Some(Mass::from_solid(&solid, &params(40.0))),
            )
            .expect("a body");
        for _ in 0..32 {
            env.step();
        }
        let (origin, _) = env.pose(body).expect("still there");
        let fallen = 1000.0 - origin.z;
        // The tolerance is one half-step of accumulated velocity —
        // 600 × 0.5 × (1/64) ≈ 4.7 units — which is the width of the band any
        // first-order stepping of `v += g·dt` can land in, whichever end of
        // the step it integrates the position at. Measured: 74.35. Both ends
        // are asserted so that a *change* of integrator is caught rather than
        // shrugged at.
        assert!(
            (70.3..79.7).contains(&fallen),
            "half a second of Portal 2 gravity should be about 75 units, was {fallen}"
        );
    }

    /// Mass does not change how fast something falls, which is the other half
    /// of the same check and the one that catches an inertia term leaking into
    /// the linear integrator.
    #[test]
    fn a_heavy_body_and_a_light_one_fall_together() {
        let mut env = Environment::new(SurfaceProps::default());
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        let mut drop = |mass: f32| {
            env.add(
                Motion::Dynamic,
                &hulls,
                Vec3::new(mass * 100.0, 0.0, 1000.0),
                Vec3::ZERO,
                "default",
                Some(Mass::from_solid(&solid, &params(mass))),
            )
            .expect("a body")
        };
        let light = drop(1.0);
        let heavy = drop(400.0);
        for _ in 0..16 {
            env.step();
        }
        let a = env.pose(light).unwrap().0.z;
        let b = env.pose(heavy).unwrap().0.z;
        assert!((a - b).abs() < 1e-3, "{a} vs {b}");
    }

    #[test]
    fn a_cube_dropped_on_a_floor_comes_to_rest_on_top_of_it() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        let cube = env
            .add(
                Motion::Dynamic,
                &hulls,
                Vec3::new(0.0, 0.0, 200.0),
                Vec3::ZERO,
                "default",
                Some(Mass::from_solid(&solid, &params(40.0))),
            )
            .expect("a cube");
        // Five seconds at 64 Hz: long enough to fall 200 units, bounce and
        // settle, and short enough that a test suite does not notice.
        for _ in 0..320 {
            env.step();
        }
        let (origin, angles) = env.pose(cube).expect("still there");
        assert!(
            (origin.z - 16.0).abs() < 1.0,
            "a 32-unit cube resting on a floor at z=0 sits at z=16, not {}",
            origin.z
        );
        assert!(
            angles.abs().max_element() < 1.0 || (angles.abs().max_element() - 90.0).abs() < 1.0,
            "a cube dropped flat should still be flat, not {angles:?}"
        );
        assert!(!env.is_awake(cube), "and it should have gone to sleep");
    }

    /// IVP's `rotation_inertia` is not a moment of inertia — the whole of
    /// `portdocs/VPHYSICS.md` §3.3 in one assertion, checked against the
    /// closed form for a cube rather than against the file.
    #[test]
    fn valves_stored_inertia_is_a_factor_of_root_two_below_the_true_one() {
        let side = 35.64_f32;
        let solid = cube_solid(side / 2.0);
        let params = params(40.0);
        let stored = Mass::from_solid(&solid, &params).inertia;
        let truth = Mass::true_inertia(&solid, &params);
        // I/m for a uniform cube is a²/6, and the mass is 40 kg.
        let closed_form = side * side / 6.0 * 40.0;
        assert!(
            (truth.x - closed_form).abs() / closed_form < 1e-3,
            "{} vs {closed_form}",
            truth.x
        );
        let ratio = stored.x / truth.x;
        assert!(
            (ratio - 0.5 * 2f32.sqrt()).abs() < 1e-3,
            "IVP should store √2/2 of the truth, stored {ratio} of it"
        );
    }

    /// `auto_check_rot_inertia` — a needle cannot spin about its long axis
    /// arbitrarily fast.
    #[test]
    fn a_degenerate_inertia_axis_is_raised_to_five_percent_of_the_vector() {
        let mut solid = cube_solid(16.0);
        solid.rotation_inertia = Vec3::new(1.0, 1.0, 0.0);
        let inertia = Mass::from_solid(&solid, &params(1.0)).inertia;
        let floor = Vec3::new(1.0, 1.0, 0.0).length() * 0.05;
        assert!((inertia.z - floor).abs() < 1e-6, "{inertia:?}");
        assert_eq!(inertia.x, 1.0, "and the other two are untouched");
    }

    /// A `QAngle` survives the trip through a quaternion, which is what every
    /// writeback depends on.
    #[test]
    fn a_source_qangle_round_trips_through_the_pose() {
        for angles in [
            Vec3::ZERO,
            Vec3::new(0.0, 90.0, 0.0),
            Vec3::new(-0.336_266, 90.488_1, -0.912_995),
            Vec3::new(30.0, -120.0, 45.0),
        ] {
            let (origin, back) = from_pose(&to_pose(Vec3::new(1.0, 2.0, 3.0), angles));
            assert!((origin - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-4);
            // Compare the *rotations*, not the triples: a `QAngle` is not
            // unique and `matrix_angles` picks its own representative.
            let a = crate::math::angle_matrix(angles);
            let b = crate::math::angle_matrix(back);
            let error = (0..3)
                .map(|i| (a.col(i) - b.col(i)).length())
                .fold(0.0f32, f32::max);
            assert!(error < 1e-3, "{angles:?} came back as {back:?}");
        }
    }

    #[test]
    fn a_removed_body_leaves_a_handle_that_resolves_to_nothing() {
        let mut env = Environment::new(SurfaceProps::default());
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        let first = env
            .add(Motion::Dynamic, &hulls, Vec3::ZERO, Vec3::ZERO, "default", None)
            .unwrap();
        env.remove(first);
        assert!(env.pose(first).is_none());
        // The slot is reused, and the stale handle must not resolve to the new
        // occupant — which is the case a cube removed by `Dissolve` in the
        // same tick the environment is stepped would otherwise hit.
        let second = env
            .add(Motion::Dynamic, &hulls, Vec3::ZERO, Vec3::ZERO, "default", None)
            .unwrap();
        assert_ne!(first, second);
        assert!(env.pose(first).is_none());
        assert!(env.pose(second).is_some());
    }

    #[test]
    fn a_frozen_body_stays_where_it_is_and_a_thawed_one_falls() {
        let mut env = Environment::new(SurfaceProps::default());
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        let cube = env
            .add(
                Motion::Dynamic,
                &hulls,
                Vec3::new(0.0, 0.0, 500.0),
                Vec3::ZERO,
                "default",
                Some(Mass::from_solid(&solid, &params(40.0))),
            )
            .unwrap();
        env.enable_motion(cube, false);
        for _ in 0..64 {
            env.step();
        }
        assert_eq!(env.pose(cube).unwrap().0.z, 500.0, "frozen");
        env.enable_motion(cube, true);
        for _ in 0..64 {
            env.step();
        }
        assert!(env.pose(cube).unwrap().0.z < 400.0, "and then falling");
    }

    /// `GetActiveObjects` is what the writeback walks, and it must not report
    /// the world.
    #[test]
    fn only_dynamic_bodies_are_reported_as_active() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        let cube = env
            .add(
                Motion::Dynamic,
                &hulls,
                Vec3::new(0.0, 0.0, 400.0),
                Vec3::ZERO,
                "default",
                Some(Mass::from_solid(&solid, &params(40.0))),
            )
            .unwrap();
        env.step();
        let active: Vec<BodyId> = env.active().map(|(id, _, _)| id).collect();
        assert_eq!(active, vec![cube]);
    }
}
