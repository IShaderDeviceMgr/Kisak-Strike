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
use rapier3d::geometry::{
    Collider, ColliderBuilder, ColliderHandle, Group, InteractionGroups, SharedShape,
};
use rapier3d::math::Pose;
use rapier3d::parry::query::{ShapeCastOptions, ShapeCastStatus};
use rapier3d::pipeline::{PhysicsWorld, QueryFilter};
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
    /// Moved by the solver, but driven every tick towards where the *game*
    /// says the player is — `CreatePlayerController`'s object.
    ///
    /// Dynamic, so that a cube it walks into is pushed and so that the world
    /// can stop it; rotation-locked, because Valve gives it an inertia of
    /// `1e24` and damps its spin by 100 to the same end; and weightless,
    /// because the player's height is decided by `client/`'s movement and
    /// gravity here would only fight it.
    ///
    /// > **[`sweep_box`](Environment::sweep_box) must not return it**, which
    /// > is why this is its own variant rather than [`Dynamic`](Motion::Dynamic)
    /// > with some flags: the player's own movement trace would otherwise be
    /// > stopped dead by the player's own shadow, one unit into every step.
    Player,
}

/// What [`Environment::sweep_box`] found: `trace_t`, cut down to the three
/// fields a vphysics clip actually fills in.
///
/// `CPhysicsCollision::TraceBox` (`physics_collide.cpp`) writes `fraction`,
/// `plane.normal` and `startsolid` into the caller's `trace_t` and leaves
/// everything else to the engine — the surface, the contents, the entity — so
/// those are what cross this boundary. Naming the *body* rather than the
/// entity is deliberate: `vphysics/` does not know entities exist, and
/// `server/physics.rs` already holds the map from one to the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sweep {
    /// How far along `start → end` the box got, in `[0, 1]`.
    pub fraction: f32,
    /// The normal of the surface it stopped against, pointing **out of what
    /// was hit** and so back towards the sweeper — Source's
    /// `trace_t::plane::normal` convention.
    ///
    /// **Zero for a zero-length sweep**, which is a position test: there is no
    /// direction, so there is no face the box can be said to have come in
    /// through. A *swept* answer always carries a real normal, including a
    /// penetrating one — `compute_impact_geometry_on_penetration` is set for
    /// exactly that.
    pub normal: Vec3,
    /// Which body stopped it.
    pub body: BodyId,
    /// `trace_t::startsolid` — the box was already inside this body before it
    /// moved at all.
    pub start_solid: bool,
}

/// One contact on a body — one step of Valve's `IPhysicsFrictionSnapshot`.
///
/// `CFrictionSnapshot` (`physics_friction.cpp`) is a cursor over an object's
/// contact points with four accessors; this is those four as a value, and
/// [`Environment::contacts`] is the loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// Pointing **from the body that was asked towards the other one** —
    /// `CFrictionSnapshot::GetSurfaceNormal`'s convention, sign included. A
    /// body resting on the floor reports a normal with `z ≈ -1`.
    pub normal: Vec3,
    /// The other body, or `None` when it is geometry with no [`BodyId`] — the
    /// world's own solids have one, so in practice this is `None` only for a
    /// body removed between the step and the question.
    pub other: Option<BodyId>,
    /// `IPhysicsObject::IsMoveable` — false for the world, for a static prop
    /// and for a frozen one.
    pub moveable: bool,
    /// `IPhysicsObject::GetMass`, and `None` for anything not moveable, which
    /// is Valve's `!pOther->IsMoveable()` reaching the same conclusion first.
    pub mass: Option<f32>,
    /// `IPhysicsFrictionSnapshot::GetNormalForce`, in kg·units/s².
    ///
    /// > Rapier accumulates an *impulse* over the step rather than a force,
    /// > so this is the impulse divided by [`TIMESTEP`]. Valve's
    /// > `IVP_Contact_Point_API::get_vert_force` is already a force.
    pub normal_force: f32,
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

    /// An axis-aligned box — `PhysCreateBbox( mins, maxs )`
    /// (`physics_shared.cpp`), which is how the player's hull is made and the
    /// only shape in the game that does not come out of a file.
    ///
    /// > **Wrapped in a compound so that the offset survives.** A Source hull
    /// > is not centred on its entity's origin — the player's is
    /// > `(-16,-16,0)`–`(16,16,72)`, because the origin is on the floor
    /// > between the feet — and a bare `SharedShape::cuboid` is centred on
    /// > whatever it is attached to. A one-child compound carries the shift,
    /// > which keeps [`Environment::add`] free of a per-shape pose it would
    /// > otherwise need for this one caller.
    pub fn from_box(mins: Vec3, maxs: Vec3) -> Hulls {
        let mut out = Hulls::default();
        let half = (maxs - mins) * 0.5;
        if half.min_element() <= 0.0 {
            out.degenerate += 1;
            return out;
        }
        let cuboid = SharedShape::cuboid(half.x, half.y, half.z);
        let pose = Pose::from_parts((mins + maxs) * 0.5, Quat::IDENTITY);
        out.shapes.push(SharedShape::compound(vec![(pose, cuboid)]));
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

/// The player's shadow, for collision filtering. One bit, because the port
/// has exactly one player.
const GROUP_PLAYER: Group = Group::GROUP_1;

/// A body the player is carrying — `COLLISION_GROUP_PLAYER_HELD`.
const GROUP_HELD: Group = Group::GROUP_2;

struct Body {
    handle: RigidBodyHandle,
    generation: u32,
    /// Whether this body was created [`Motion::Dynamic`].
    ///
    /// Not the same question as `RigidBody::body_type()`, and that is the
    /// point: `EnableMotion( false )` makes a dynamic body *fixed*, which is
    /// exactly what the world's own bodies are. Only the creating call can
    /// tell a frozen cube from a wall, so it is recorded then and
    /// [`sweep_box`](Environment::sweep_box) reads it back.
    dynamic: bool,
    /// Whether the player is carrying this body right now —
    /// `FVPHYSICS_PLAYER_HELD` and `COLLISION_GROUP_PLAYER_HELD` together.
    ///
    /// Read by [`sweep_box`](Environment::sweep_box), which must **not**
    /// report it: the object is held fifteen units in front of the eye, so
    /// the player's own movement trace would otherwise be stopped by the cube
    /// in their hands and they could not walk forwards. Valve reaches the
    /// same place with `CTraceFilterSkipTwoEntities` at every call site; one
    /// flag on the body is the same rule stated once.
    ///
    /// The solver needs the rule as well as the trace, and that half is
    /// collider interaction groups — see [`set_held`](Environment::set_held).
    held: bool,
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
            Motion::Dynamic | Motion::Player => RigidBodyBuilder::dynamic(),
        };
        let mut builder = builder.pose(pose).user_data(id.to_user_data());
        if motion == Motion::Player {
            // `AttachObject`'s `rot_speed_damp_factor = (100,100,100)` and
            // `SetupVPhysicsShadow`'s `solid.params.inertia = 1e24`, which are
            // two ways of saying the same thing, plus `EnableGravity( false )`
            // — see [`Motion::Player`].
            builder = builder.lock_rotations().gravity_scale(0.0);
        }
        if let Some(mass) = mass {
            builder = builder
                .additional_mass_properties(MassProperties::new(mass.center, mass.mass, mass.inertia))
                .linear_damping(mass.damping)
                .angular_damping(mass.rot_damping);
        }
        let handle = self.world.insert_body(builder);

        let surface = self.surfaces.resolve(surface).clone();
        // The player's shadow is the only body that needs a membership of its
        // own, and it needs one so that a *held* object can name it in a
        // filter — see [`set_held`](Environment::set_held). Everything else
        // keeps Rapier's default of "in every group, collides with every
        // group", which is what `COLLISION_GROUP_NONE` means.
        let groups = match motion {
            Motion::Player => InteractionGroups::all().with_memberships(GROUP_PLAYER),
            _ => InteractionGroups::all(),
        };
        for shape in &hulls.shapes {
            let mut collider = collider(shape.clone(), &surface);
            collider.set_collision_groups(groups);
            self.world.insert_collider(collider, Some(handle));
        }
        // **Fold the mass properties in now rather than at the first step.**
        // Rapier recomputes `local_mprops` from the colliders and the
        // additional properties during `step`, so until then a body created
        // this tick reports a mass of zero — and
        // [`mass`](Environment::mass) and [`set_mass`](Environment::set_mass)
        // would both answer about a body that does not weigh anything yet.
        // Nothing depended on that while mass was read-only; `grab` saves the
        // mass it is about to replace, and would save the zero.
        if let Some(body) = self.world.bodies.get_mut(handle) {
            body.recompute_mass_properties_from_colliders(&self.world.colliders);
        }
        self.bodies[id.slot as usize] = Some(Body {
            handle,
            generation: id.generation,
            dynamic: matches!(motion, Motion::Dynamic),
            held: false,
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

    /// A body's linear velocity, and the setter the player's controller
    /// drives it with.
    ///
    /// `IVP_Core::speed` read and written, which is what
    /// `ComputeController` does to it directly in the shipped tree — the
    /// controller runs *inside* the solver there and has the core in hand.
    pub fn velocity(&self, id: BodyId) -> Vec3 {
        self.handle(id)
            .and_then(|handle| self.world.bodies.get(handle))
            .map_or(Vec3::ZERO, |body| body.linvel())
    }

    /// See [`velocity`](Environment::velocity).
    pub fn set_velocity(&mut self, id: BodyId, velocity: Vec3) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        if let Some(body) = self.world.bodies.get_mut(handle) {
            body.set_linvel(velocity, true);
        }
    }

    /// A body's angular velocity, in radians per second about each world
    /// axis, and the setter the grab controller drives it with.
    ///
    /// `IVP_Core::rot_speed`, the companion to
    /// [`velocity`](Environment::velocity). Nothing needed it until
    /// [`grab`](super::grab): the player's own shadow is rotation-locked
    /// ([`Motion::Player`]) and a mover's spin is written as a pose, so this
    /// is the first controller here that steers a body's *rotation* through
    /// the solver.
    pub fn angular_velocity(&self, id: BodyId) -> Vec3 {
        self.handle(id)
            .and_then(|handle| self.world.bodies.get(handle))
            .map_or(Vec3::ZERO, |body| body.angvel())
    }

    /// See [`angular_velocity`](Environment::angular_velocity).
    pub fn set_angular_velocity(&mut self, id: BodyId, angular: Vec3) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        if let Some(body) = self.world.bodies.get_mut(handle) {
            body.set_angvel(angular, true);
        }
    }

    /// A body's orientation as a quaternion.
    ///
    /// [`pose`](Environment::pose) answers the same question as a Source
    /// `QAngle`, which is what an *entity* wants. This is for the caller that
    /// wants to take a difference: `QuaternionDiff` composed with
    /// `QuaternionAxisAngle` is how `ComputeShadowControllerIVP`
    /// (`physics_shadow.cpp:826`) turns "you are here, go there" into an
    /// angular error, and routing that through Euler angles and back would
    /// lose the shortest-arc property that makes it work.
    pub fn rotation(&self, id: BodyId) -> Option<Quat> {
        let handle = self.handle(id)?;
        Some(self.world.bodies.get(handle)?.position().rotation)
    }

    /// `IPhysicsObject::SetMass`, keeping the body's *shape* of inertia and
    /// scaling its magnitude with the mass.
    ///
    /// `CGrabController::AttachEntity` drops a held object to
    /// [`CARRY_MASS`](super::grab::CARRY_MASS) and puts the original back on
    /// detach, which is the whole reason a held cube cannot fling the player.
    ///
    /// > **Inertia scales with the mass and the shape does not.** Valve's
    /// > `CPhysicsObject::SetMass` does `SetInertia( m_pObject->get_rot_inertia()
    /// > * (mass / m_pObject->get_mass()) )` — the *ratio*, not a recomputation
    /// > — because the inertia tensor of a rigid shape is linear in its mass.
    /// > Recomputing it from the hulls would also throw away
    /// > [`Mass::from_solid`]'s deliberate reproduction of IVP's
    /// > `rotation_inertia` bug, and every throw in Portal 2 is tuned against
    /// > that (`portdocs/VPHYSICS.md` §0).
    pub fn set_mass(&mut self, id: BodyId, mass: f32) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        let Some(body) = self.world.bodies.get_mut(handle) else {
            return;
        };
        let current = body.mass();
        if current <= 0.0 || mass <= 0.0 {
            return;
        }
        let props = body.mass_properties().local_mprops;
        let ratio = mass / current;
        let inertia = props.reconstruct_inertia_matrix() * ratio;
        body.set_additional_mass_properties(
            MassProperties::with_inertia_matrix(props.local_com, mass, inertia),
            true,
        );
        // As in [`add`](Environment::add): the effective properties are only
        // folded together during a step, and a caller that sets a mass and
        // reads it back in the same tick — which is exactly what attaching and
        // detaching a grab controller does — would see the old one.
        // The colliders contribute nothing (they are built at density 0), so
        // this resolves to precisely what was just set.
        body.recompute_mass_properties_from_colliders(&self.world.colliders);
    }

    /// `IPhysicsObject::SetDamping`'s rotational half.
    ///
    /// `AttachEntity` raises a held object's rotational damping to 10 so that
    /// it stops tumbling in the player's hands; `DetachEntity` restores what
    /// the `.phy`'s `SolidParams` asked for.
    pub fn angular_damping(&self, id: BodyId) -> f32 {
        self.handle(id)
            .and_then(|handle| self.world.bodies.get(handle))
            .map_or(0.0, |body| body.angular_damping())
    }

    /// See [`angular_damping`](Environment::angular_damping).
    pub fn set_angular_damping(&mut self, id: BodyId, damping: f32) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        if let Some(body) = self.world.bodies.get_mut(handle) {
            body.set_angular_damping(damping);
        }
    }

    /// Does `body`'s own collision overlap the box `half` at `at`?
    ///
    /// `TestIntersectionVsHeldObjectCollide`'s `SOLID_BBOX` arm
    /// (`portal_grabcontroller_shared.cpp:2130`), which is the question
    /// `CGrabController::DetachEntity` asks before it lets go: *would putting
    /// this down leave it inside the player?* If it would, the drop is
    /// refused and the hold continues.
    ///
    /// **One named body, not the world.** [`sweep_box`](Environment::sweep_box)
    /// answers "what is in the way" over every dynamic prop; this answers "is
    /// it *that* one", which is what the drop test needs — a cube overlapping
    /// some *other* prop is not a reason to refuse.
    pub fn overlaps_box(&self, body: BodyId, half: Vec3, at: Vec3) -> bool {
        let Some(handle) = self.handle(body) else {
            return false;
        };
        let Some(rigid) = self.world.bodies.get(handle) else {
            return false;
        };
        if half.min_element() < 0.0 {
            return false;
        }
        let shape = SharedShape::cuboid(half.x, half.y, half.z);
        let pose = Pose::from_parts(at, Quat::IDENTITY);
        rigid.colliders().iter().any(|&collider| {
            self.world.colliders.get(collider).is_some_and(|other| {
                rapier3d::parry::query::intersection_test(
                    &pose,
                    shape.as_ref(),
                    other.position(),
                    other.shape(),
                )
                .unwrap_or(false)
            })
        })
    }

    /// A body's collision bounds **in its own frame** — `CCollisionProperty`'s
    /// `OBBMins`/`OBBMaxs`.
    ///
    /// Three callers want it and all three are the grab controller's:
    /// `CanPickupObject`'s 128-unit size limit, `BoundingRadius()` for the
    /// carry stand-off, and `m_attachedPositionObjectSpace` — the object's
    /// *centre*, which is what makes a carry target an origin rather than a
    /// centre.
    ///
    /// > **Taken from the collision model rather than from the studio model.**
    /// > Valve reads `CBaseEntity::CollisionProp()`, which for a
    /// > `SOLID_VPHYSICS` prop is built from the `.phy` — the same hulls these
    /// > colliders are. `server/` never reads a `.mdl`'s bounds for a prop at
    /// > all (`EntityCore::model_bounds` is only filled for brush models), so
    /// > this is also the only answer available here.
    pub fn local_bounds(&self, id: BodyId) -> Option<(Vec3, Vec3)> {
        let handle = self.handle(id)?;
        let body = self.world.bodies.get(handle)?;
        let (mut mins, mut maxs) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        let mut any = false;
        for &handle in body.colliders() {
            let Some(collider) = self.world.colliders.get(handle) else {
                continue;
            };
            // The collider's own pose *within* the body, which is identity for
            // every hull this port builds — but reading it costs nothing and
            // a compound placed off-centre would otherwise be wrong.
            let aabb = collider.shape().compute_aabb(collider.position_wrt_parent()?);
            mins = mins.min(aabb.mins);
            maxs = maxs.max(aabb.maxs);
            any = true;
        }
        any.then_some((mins, maxs))
    }

    /// `IPhysicsObject::SetCollisionGroup( COLLISION_GROUP_PLAYER_HELD )`
    /// (`const.h:410`) — *"Held objects that shouldn't collide with players"*.
    ///
    /// The player's shadow is a **dynamic** body here
    /// (`portdocs/VPHYSICS_SHADOW.md` §3), so a 1 kg cube held fifteen units
    /// in front of an 85 kg driven one would be in permanent contact with it
    /// and would fight the controller every step. Valve names a collision
    /// group and lets a table decide; Rapier puts the table on the collider as
    /// a membership/filter pair, which is the same idea with the indices
    /// swapped.
    pub fn set_held(&mut self, id: BodyId, held: bool) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        let colliders: Vec<_> = match self.world.bodies.get(handle) {
            Some(body) => body.colliders().to_vec(),
            None => return,
        };
        let groups = match held {
            true => InteractionGroups::all()
                .with_memberships(GROUP_HELD)
                .with_filter(Group::ALL & !GROUP_PLAYER),
            false => InteractionGroups::all(),
        };
        if let Some(body) = self.bodies.get_mut(id.slot as usize).and_then(Option::as_mut) {
            body.held = held;
        }
        for handle in colliders {
            if let Some(collider) = self.world.colliders.get_mut(handle) {
                collider.set_collision_groups(groups);
            }
        }
    }

    /// `IVP_Real_Object::beam_object_to_new_position` — put a body somewhere
    /// without sweeping it there.
    ///
    /// `CPlayerController::TryTeleportObject` disables collision detection
    /// across the call so that the beam cannot generate an impulse against
    /// whatever the body lands in. Rapier's `set_position` is not a swept
    /// move and generates no contact of its own, so there is nothing to
    /// disable: the next narrow phase sees the new pose and nothing sees the
    /// transit.
    pub fn teleport(&mut self, id: BodyId, origin: Vec3) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        if let Some(body) = self.world.bodies.get_mut(handle) {
            let pose = Pose::from_parts(origin, body.position().rotation);
            body.set_position(pose, true);
        }
    }

    /// Replaces a body's shape, keeping its pose, its velocity and its id.
    ///
    /// `IPhysicsPlayerController::SetObject`, which the shipped game uses for
    /// exactly one thing: swapping the player between the standing hull and
    /// the crouching one (`CBasePlayer::SetVCollisionState`). Valve builds
    /// *two objects* and moves the controller between them; one body whose
    /// colliders are replaced is the same thing with one fewer identity to
    /// keep in step.
    ///
    /// > **It does not check that the new hull fits.** Neither does Valve's:
    /// > `CGameMovement::CanUnduck` has already asked that question on the
    /// > trace side, and by the time this is called the player's own origin
    /// > has already moved.
    pub fn set_hulls(&mut self, id: BodyId, hulls: &Hulls, surface: &str) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        let existing: Vec<_> = match self.world.bodies.get(handle) {
            Some(body) => body.colliders().to_vec(),
            None => return,
        };
        for collider in existing {
            self.world.remove_collider(collider);
        }
        let surface = self.surfaces.resolve(surface).clone();
        for shape in &hulls.shapes {
            let collider = collider(shape.clone(), &surface);
            self.world.insert_collider(collider, Some(handle));
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

    /// Sweeps an axis-aligned box from `start` to `end` against the physics
    /// props, and reports the first one it meets.
    ///
    /// This is `CEngineTrace::ClipRayToVPhysics` (`enginetrace.cpp:1115`)
    /// minus the dispatch that gets there: the shipped engine reaches it once
    /// per `SOLID_VPHYSICS` entity the spatial partition offers, asks that
    /// entity's physics object for its `CPhysCollide`, and calls
    /// `physcollision->TraceBox`. Here the broad phase is Rapier's and one
    /// call covers every prop at once, so there is no per-entity loop to
    /// write.
    ///
    /// `half` is the box's half-extents; the box is centred on `start` and
    /// stays axis-aligned, because Source's `Ray_t` for a hull trace is an
    /// AABB sweep and every caller here is one. A hull that is not centred on
    /// its entity's origin — the player's is not — is the caller's to shift,
    /// exactly as `Ray_t::Init` shifts it.
    ///
    /// > **Only bodies created [`Motion::Dynamic`], and that is correctness
    /// > rather than thrift.** The world, its displacements, its static props
    /// > and its brush entities are all in this environment *and* all already
    /// > in `trace/`. A sweep that returned them would clip the same geometry
    /// > twice, with two implementations that do not have to agree — and the
    /// > brush entities' half would be the worse answer of the two, because
    /// > `trace/` carves portal holes out of them and this does not.
    ///
    /// > **The test is `Body::dynamic`, not `RigidBody::body_type()`.**
    /// > `EnableMotion( false )` freezes a prop by making it a *fixed* body
    /// > (see [`enable_motion`](Environment::enable_motion)), which is
    /// > indistinguishable from a wall by type alone. Filtering on the type
    /// > would make a frozen cube stop stopping the player — and every cube in
    /// > the game spawns frozen if its map says so.
    pub fn sweep_box(&self, half: Vec3, start: Vec3, end: Vec3) -> Option<Sweep> {
        if half.min_element() < 0.0 {
            return None;
        }
        let delta = end - start;
        let shape = SharedShape::cuboid(half.x, half.y, half.z);
        let pose = Pose::from_parts(start, Quat::IDENTITY);
        // `max_time_of_impact = 1` with the velocity set to the whole
        // displacement makes the reported time *be* the fraction, which is the
        // number `trace_t` wants and saves dividing by a length that can be
        // zero. A zero-length sweep is therefore still a legitimate query —
        // it is Source's `startsolid` test — and comes back with a fraction of
        // 0 and the flag set.
        let options = ShapeCastOptions {
            max_time_of_impact: 1.0,
            target_distance: 0.0,
            // **`false`, and this is the whole of why a player can walk out of
            // a cube again.** With `true` a sweep that *starts* overlapping
            // reports a time of impact of zero whatever direction it is going
            // — including straight away from the thing it is inside — so a
            // player who ends up inside a prop for one tick is trapped there
            // for good, with `fraction == 0` in all six directions and no
            // `CheckStuck` in this port to nudge them out.
            //
            // `false` discards a time-zero impact whose relative velocity is
            // *separating*, which is the same thing Valve's brush sweep gets
            // for free: `CM_ClipBoxToBrush` tests planes offset by
            // `DIST_EPSILON`, so a box leaving a brush it is inside is never
            // stopped by it. Approaching still collides, so nothing gets
            // easier to walk through.
            stop_at_penetration: false,
            compute_impact_geometry_on_penetration: true,
        };
        let is_prop = |_: ColliderHandle, collider: &Collider| -> bool {
            self.body_of(collider)
                .is_some_and(|body| body.dynamic && !body.held)
        };
        let filter = QueryFilter::default()
            .exclude_sensors()
            .predicate(&is_prop);
        // **A zero-length sweep takes a different query.** Source asks for
        // these constantly — `CM_UnsweptBoxTrace`, and every "am I stuck"
        // test — and `cast_shape` answers `None` for them: with no velocity
        // there is no time of impact to find, and `stop_at_penetration`'s
        // separating-velocity test has nothing to judge. So the position test
        // is an intersection test, which is what it is.
        if delta.length_squared() <= 0.0 {
            let (_, collider) = self
                .world
                .intersect_shape(pose, &*shape, filter)
                .next()?;
            return Some(Sweep {
                fraction: 0.0,
                // No contact plane: the shapes overlap, and which face of the
                // prop is "the" one the box came through is not a question an
                // intersection test can answer. `Sweep::normal` documents the
                // zero.
                normal: Vec3::ZERO,
                body: self.id_of(collider)?,
                start_solid: true,
            });
        }
        let (collider, hit) = self
            .world
            .cast_shape(&pose, delta, &*shape, options, filter)?;
        let body = self
            .world
            .colliders
            .get(collider)
            .and_then(|collider| self.id_of(collider))?;
        Some(Sweep {
            // **`normal1`, and that is not what parry's field names suggest.**
            // Read literally, `normal1` is the outward normal on the *first*
            // shape — the moving box — which for a box swept east into a wall
            // points east, and Source's `plane.normal` points west. Rapier's
            // query pipeline casts the collider against the shape and flips
            // the result, so the two are exchanged by the time they arrive
            // here. Asserted by `the_sweep_normal_points_back_at_the_sweeper`,
            // which is the only reason this is knowable.
            normal: hit.normal1,
            fraction: hit.time_of_impact.clamp(0.0, 1.0),
            body,
            start_solid: hit.status == ShapeCastStatus::PenetratingOrWithinTargetDist,
        })
    }

    /// The [`BodyId`] a collider belongs to, or `None` if its body has been
    /// removed and its slot reused since the handle was issued.
    fn id_of(&self, collider: &Collider) -> Option<BodyId> {
        let handle = collider.parent()?;
        let id = BodyId::from_user_data(self.world.bodies.get(handle)?.user_data);
        self.slot(id).map(|_| id)
    }

    fn body_of(&self, collider: &Collider) -> Option<&Body> {
        let handle = collider.parent()?;
        let id = BodyId::from_user_data(self.world.bodies.get(handle)?.user_data);
        self.slot(id)
    }

    /// The live record for a handle — `None` once the body has been removed,
    /// which is what makes [`BodyId`] generational rather than an index.
    fn slot(&self, id: BodyId) -> Option<&Body> {
        self.bodies
            .get(id.slot as usize)?
            .as_ref()
            .filter(|body| body.generation == id.generation)
    }

    /// Every contact a body currently has — `IPhysicsFrictionSnapshot`, which
    /// is `CreateFrictionSnapshot` and the `while ( pSnapshot->IsValid() )`
    /// loop around it (`physics_friction.cpp:144`), collected in one go.
    ///
    /// Returned by value rather than as an iterator because the one caller —
    /// [`PlayerController`](super::shadow::PlayerController) — needs `&mut
    /// Environment` immediately afterwards to act on what it found, and
    /// because a body in this game has a handful of contacts rather than a
    /// stream of them.
    ///
    /// **After a step, not before.** Rapier's narrow phase fills the contact
    /// set during `step`, so this answers about the *last* step. That is also
    /// what Valve's does: `do_simulation_controller` runs inside the
    /// simulation and reads contacts the previous PSI established.
    pub fn contacts(&self, id: BodyId) -> Vec<Contact> {
        let Some(handle) = self.handle(id) else {
            return Vec::new();
        };
        let Some(body) = self.world.bodies.get(handle) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &own in body.colliders() {
            for pair in self.world.contact_pairs_with(own) {
                let mine_is_first = pair.collider1 == own;
                let theirs = match mine_is_first {
                    true => pair.collider2,
                    false => pair.collider1,
                };
                let Some(other) = self.world.colliders.get(theirs) else {
                    continue;
                };
                let other_body = other.parent().and_then(|h| self.world.bodies.get(h));
                let moveable = other_body.is_some_and(|b| b.is_dynamic());
                let mass = other_body.filter(|b| b.is_dynamic()).map(|b| b.mass());
                let id = self.id_of(other);
                for manifold in &pair.manifolds {
                    if manifold.points.is_empty() {
                        continue;
                    }
                    // `CFrictionSnapshot::GetSurfaceNormal`'s
                    // `out *= sign[m_synapseIndex]`: the normal is reported
                    // **pointing from the asking object towards the other
                    // one**, which is why Valve's ground test reads
                    // `normal.z < -0.7` rather than `> 0.7`. Rapier's is
                    // collider1 → collider2, so it is the same vector when we
                    // are collider1 and its negation when we are not.
                    let normal = match mine_is_first {
                        true => manifold.data.normal,
                        false => -manifold.data.normal,
                    };
                    out.push(Contact {
                        normal,
                        other: id,
                        moveable,
                        mass,
                        normal_force: pair.total_impulse_magnitude() / TIMESTEP,
                    });
                }
            }
        }
        out
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
        self.slot(id).map(|body| body.handle)
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

    /// A cube of half-extent 16 at `origin`, as a dynamic prop.
    fn prop(env: &mut Environment, origin: Vec3) -> BodyId {
        let solid = cube_solid(16.0);
        let hulls = Hulls::from_solid(&solid);
        env.add(
            Motion::Dynamic,
            &hulls,
            origin,
            Vec3::ZERO,
            "default",
            Some(Mass::from_solid(&solid, &params(40.0))),
        )
        .expect("a prop")
    }

    /// The sweep is for **props**, and the world is already in `trace/`.
    ///
    /// A floor and a cube in the same environment: a box swept down through
    /// both must report the cube and must not report the floor, because
    /// `trace/` will clip against its own copy of the floor and clipping twice
    /// against two implementations is how the two get to disagree.
    #[test]
    fn a_sweep_reports_the_prop_and_not_the_world() {
        let mut env = Environment::new(SurfaceProps::default());
        let ground = floor(&mut env);
        let cube = prop(&mut env, Vec3::new(0.0, 0.0, 100.0));
        env.step();

        let hit = env
            .sweep_box(
                Vec3::splat(1.0),
                Vec3::new(0.0, 0.0, 300.0),
                Vec3::new(0.0, 0.0, -32.0),
            )
            .expect("the cube is in the way");
        assert_eq!(hit.body, cube, "the cube, not the floor");
        assert_ne!(hit.body, ground);

        // …and with the cube gone, the floor is still invisible to it.
        env.remove(cube);
        env.step();
        assert_eq!(
            env.sweep_box(
                Vec3::splat(1.0),
                Vec3::new(0.0, 0.0, 300.0),
                Vec3::new(0.0, 0.0, -32.0),
            ),
            None,
            "the world is `trace/`'s job"
        );
    }

    /// **A frozen prop still stops a sweep**, which is why the filter reads
    /// `Body::dynamic` rather than `RigidBody::body_type()`.
    ///
    /// `EnableMotion( false )` makes a prop a *fixed* body, indistinguishable
    /// by type from the world; filtering on the type would let the player walk
    /// through every cube a map spawns frozen.
    #[test]
    fn a_frozen_prop_still_stops_a_sweep() {
        let mut env = Environment::new(SurfaceProps::default());
        let cube = prop(&mut env, Vec3::new(0.0, 0.0, 100.0));
        env.enable_motion(cube, false);
        env.step();

        let hit = env.sweep_box(
            Vec3::splat(1.0),
            Vec3::new(0.0, 0.0, 300.0),
            Vec3::new(0.0, 0.0, 0.0),
        );
        assert_eq!(hit.map(|hit| hit.body), Some(cube));
    }

    /// The player's own shadow must not stop the player's own movement trace,
    /// which is the whole reason [`Motion::Player`] is a variant rather than a
    /// flag on [`Motion::Dynamic`].
    #[test]
    fn the_players_shadow_is_not_swept_against() {
        let mut env = Environment::new(SurfaceProps::default());
        let hulls = Hulls::from_box(Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0));
        env.add(
            Motion::Player,
            &hulls,
            Vec3::new(0.0, 0.0, 100.0),
            Vec3::ZERO,
            "default",
            Some(Mass {
                mass: 85.0,
                center: Vec3::ZERO,
                inertia: Vec3::splat(85.0),
                damping: 0.0,
                rot_damping: 0.0,
            }),
        )
        .expect("a shadow");
        env.step();

        assert_eq!(
            env.sweep_box(
                Vec3::splat(1.0),
                Vec3::new(0.0, 0.0, 300.0),
                Vec3::new(0.0, 0.0, 0.0),
            ),
            None
        );
    }

    /// Source's `trace_t::plane::normal` points **out of the surface hit**, so
    /// a box swept east into a cube gets a westward normal.
    #[test]
    fn the_sweep_normal_points_back_at_the_sweeper() {
        let mut env = Environment::new(SurfaceProps::default());
        prop(&mut env, Vec3::new(200.0, 0.0, 0.0));
        env.step();

        let hit = env
            .sweep_box(Vec3::splat(2.0), Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0))
            .expect("the cube is in the way");
        assert!(
            hit.normal.x < -0.9,
            "swept +x into a cube, expected a -x normal, got {:?}",
            hit.normal
        );
        // 200 - 16 (the cube) - 2 (the box) = 182 of the 400 swept.
        assert!(
            (hit.fraction - 182.0 / 400.0).abs() < 0.01,
            "fraction {}",
            hit.fraction
        );
        assert!(!hit.start_solid);
    }

    /// **A sweep that begins inside a prop and moves *out* of it is not
    /// blocked, and one that moves further *in* is.**
    ///
    /// This is the fix for a player who walked into the cube on
    /// `sp_a1_intro1` and could not walk away again: with
    /// `stop_at_penetration` set, every sweep from a penetrating start
    /// reported a fraction of zero whatever direction it was going, so the
    /// move was zeroed in all six directions and nothing but `noclip` got the
    /// player out. There is no `CheckStuck` in this port to nudge them.
    ///
    /// Valve's brush sweep has the same property for free — `CM_ClipBoxToBrush`
    /// offsets its planes by `DIST_EPSILON`, so a box leaving a brush it is
    /// inside is never stopped by it.
    #[test]
    fn a_sweep_can_leave_a_prop_it_starts_inside() {
        let mut env = Environment::new(SurfaceProps::default());
        let cube = prop(&mut env, Vec3::ZERO);
        env.step();

        // Eight units inside the cube's +x face, which is at x = 16.
        let inside = Vec3::new(8.0, 0.0, 0.0);
        // Straight out through the near face.
        assert_eq!(
            env.sweep_box(Vec3::splat(2.0), inside, inside + Vec3::new(400.0, 0.0, 0.0)),
            None,
            "a box already inside a prop must be free to leave it"
        );
        // Straight further in.
        let deeper = env
            .sweep_box(Vec3::splat(2.0), inside, inside - Vec3::new(400.0, 0.0, 0.0))
            .expect("driving deeper into a prop still collides");
        assert_eq!(deeper.body, cube);
        assert!(deeper.start_solid);
        assert_eq!(deeper.fraction, 0.0);

        // …and the *position* test still reports the overlap, because that is
        // what `startsolid` means and nothing about it depends on a direction.
        let here = env
            .sweep_box(Vec3::splat(2.0), inside, inside)
            .expect("a position test inside a prop is start-solid");
        assert!(here.start_solid);
        assert_eq!(here.fraction, 0.0);
    }

    /// A zero-length sweep is a *position test* — Source asks them constantly
    /// — and must not divide by the length it has not got.
    #[test]
    fn a_zero_length_sweep_is_a_position_test() {
        let mut env = Environment::new(SurfaceProps::default());
        prop(&mut env, Vec3::ZERO);
        env.step();

        let inside = env.sweep_box(Vec3::splat(2.0), Vec3::ZERO, Vec3::ZERO);
        assert!(inside.is_some_and(|hit| hit.start_solid));
        let clear = env.sweep_box(
            Vec3::splat(2.0),
            Vec3::new(500.0, 0.0, 0.0),
            Vec3::new(500.0, 0.0, 0.0),
        );
        assert_eq!(clear, None);
    }

    /// **A zero-extent sweep is a ray**, and `SharedShape::cuboid(0,0,0)` is a
    /// degenerate shape that has to survive it.
    ///
    /// Nothing in the port currently asks — `Tracer::with_props` is attached
    /// only to the player's movement, whose rays are all `Ray::hull` — but the
    /// API allows it and a `NaN` out of a degenerate support function would
    /// come back as a fraction the clip chain would believe.
    #[test]
    fn a_ray_against_a_prop_is_a_zero_extent_sweep() {
        let mut env = Environment::new(SurfaceProps::default());
        let cube = prop(&mut env, Vec3::new(200.0, 0.0, 0.0));
        env.step();

        let hit = env
            .sweep_box(Vec3::ZERO, Vec3::ZERO, Vec3::new(400.0, 0.0, 0.0))
            .expect("the cube is in the way");
        assert_eq!(hit.body, cube);
        assert!(hit.fraction.is_finite() && hit.normal.is_finite());
        // 200 - 16 of the 400 swept, with no box to expand by.
        assert!(
            (hit.fraction - 184.0 / 400.0).abs() < 0.01,
            "fraction {}",
            hit.fraction
        );
        assert!(hit.normal.x < -0.9, "normal {:?}", hit.normal);

        // …and one that misses stays a miss rather than becoming a NaN.
        assert_eq!(
            env.sweep_box(Vec3::ZERO, Vec3::new(0.0, 500.0, 0.0), Vec3::new(400.0, 500.0, 0.0)),
            None
        );
    }

    /// A body resting on a floor reports a contact whose normal points
    /// **down**, which is `CFrictionSnapshot::GetSurfaceNormal`'s convention
    /// and the reason Valve's ground test reads `normal.z < -0.7`.
    #[test]
    fn a_resting_bodys_contact_normal_points_at_what_it_rests_on() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let cube = prop(&mut env, Vec3::new(0.0, 0.0, 17.0));
        for _ in 0..192 {
            env.step();
        }
        let contacts = env.contacts(cube);
        assert!(!contacts.is_empty(), "a cube on a floor touches it");
        assert!(
            contacts.iter().all(|contact| contact.normal.z < -0.7),
            "expected downward normals, got {:?}",
            contacts.iter().map(|c| c.normal).collect::<Vec<_>>()
        );
        assert!(
            contacts.iter().all(|contact| !contact.moveable),
            "the floor is not moveable"
        );
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
