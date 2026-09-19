//! The player's physics shadow — `legacy/vphysics/physics_shadow.cpp`'s
//! `CPlayerController`, and nothing else from that file.
//!
//! `portdocs/VPHYSICS_SHADOW.md` is the design; this is the half of it that is
//! a *port* rather than a replacement.
//!
//! # What a player controller is for
//!
//! Not for moving the player. `client/`'s movement decides where the player
//! is, by tracing a hull against the world — and since
//! [`Tracer::with_props`](crate::engine::trace::Tracer::with_props) that trace
//! includes the physics props, so a cube already stops the player without any
//! of this. What a controller adds is the *other* direction: a body in the
//! environment, dragged to where the player now is, which shoves a cube out of
//! the way when the player walks into one.
//!
//! So the player's position is an **input** here and never an output. The body
//! is allowed to lag, to be stopped by geometry the movement code walked
//! through, and to be wrong; when it is wrong by more than
//! [`TELEPORT_DISTANCE`] it is put back.
//!
//! # Why it is a dynamic body and not a kinematic one
//!
//! A kinematic body pushes with unbounded force and cannot be stopped. Valve's
//! is dynamic, with its *velocity* — not its pose — written each step, and
//! with two limits on what it may do: it will not push anything heavier than
//! [`PUSH_MASS_LIMIT`], and it will not push anything faster than
//! [`PUSH_SPEED_LIMIT`]. Those two numbers are why a cube slides rather than
//! flies, and they cannot be expressed on a kinematic body at all.

use glam::Vec3;

use super::env::{BodyId, Environment, Hulls, Mass, Motion};

/// `m_maxDeltaPosition = ConvertDistanceToIVP( 24 )`
/// (`physics_shadow.cpp:291`) — how far the body may drift from the player
/// before it is put back rather than driven back.
pub const TELEPORT_DISTANCE: f32 = 24.0;

/// `SetPushMassLimit( 350.0f )` (`player.cpp:8394`), in kilograms.
///
/// > **Nothing in Portal 2 reaches it.** The heaviest collision model any
/// > ported class places is `mp_ball` at 75 kg and the cube is 40; see
/// > `portdocs/VPHYSICS_SHADOW.md` §7. It is here because the same test is
/// > what refuses to push the *world*, which is reached constantly.
pub const PUSH_MASS_LIMIT: f32 = 350.0;

/// `SetPushSpeedLimit( 50.0f )` (`player.cpp:8395`), in units per second.
///
/// Against a walk of 175 u/s this is active for every step the player takes
/// into a cube, and it is what makes the cube slide instead of fly.
pub const PUSH_SPEED_LIMIT: f32 = 50.0;

/// `solid.params.mass = 85.0f` (`player.cpp:8375`), in kilograms.
pub const PLAYER_MASS: f32 = 85.0;

/// `MAX_LIST_NORMALS` (`physics_shadow.cpp:419`).
const MAX_NORMALS: usize = 8;

/// `m_dampFactor = 1.0f` (`physics_shadow.cpp:292`).
///
/// At exactly 1 the damping term of [`compute_controller`] cancels the whole
/// of the current velocity, so what the controller writes each step is the
/// velocity needed to close the gap in one step and nothing else. Valve never
/// sets it to anything else for the player.
const DAMP_FACTOR: f32 = 1.0;

/// `CPlayerController` — the player as a body in the environment.
pub struct PlayerController {
    body: BodyId,
    /// The hull the body currently wears, as the caller's mins/maxs.
    ///
    /// `CBasePlayer::SetupVPhysicsShadow` builds **two whole objects**, one
    /// per hull, and `SetVCollisionState` moves the controller between them.
    /// Here the live hull arrives every frame anyway — it is
    /// `PlayerState::mins`/`maxs`, which the client already sends because it
    /// changes when the player ducks — so the body keeps one shape and
    /// [`set_bounds`](PlayerController::set_bounds) rebuilds it on the tick
    /// the numbers change. That is a cuboid a few times a minute against two
    /// bodies for the whole level, and it does not have to know how many hulls
    /// a player can have.
    bounds: (Vec3, Vec3),
    /// `m_maxSpeed` — the per-axis cap on what one step may add, recomputed by
    /// [`max_speed`] from the velocity the game asked for.
    max_speed: Vec3,
    /// `m_lastImpulse`, kept for the same reason Valve keeps it: a tick the
    /// game did not update is limited to what the last updated one did.
    last_impulse: Vec3,
    /// `m_enable` — false while the player is asking to stand still.
    enabled: bool,
    /// Whether the last drive found the body touching something the solver is
    /// also moving — `IPhysicsPlayerController::IsInContact`.
    in_contact: bool,
}

impl PlayerController {
    /// `CBasePlayer::SetupVPhysicsShadow` (`player.cpp:8370`) plus
    /// `physenv->CreatePlayerController`.
    ///
    /// `mins`/`maxs` are the hull relative to `origin` — which sits on the
    /// floor between the feet, so it is not centred and [`Hulls::from_box`]
    /// carries the shift.
    ///
    /// Returns `None` if the environment refused the body, which for a box
    /// hull means the box was degenerate.
    pub fn new(
        env: &mut Environment,
        origin: Vec3,
        mins: Vec3,
        maxs: Vec3,
    ) -> Option<PlayerController> {
        let hulls = Hulls::from_box(mins, maxs);
        // `g_PhysDefaultObjectParams` with `mass = 85`, `inertia = 1e24` and
        // `dragCoefficient = 0`. The inertia is [`Motion::Player`]'s locked
        // rotation, so what is left is the mass and no damping of either kind.
        let mass = Mass {
            mass: PLAYER_MASS,
            center: Vec3::ZERO,
            // Unused: the body cannot rotate. Written as something finite
            // rather than Valve's 1e24 so that nothing downstream has to cope
            // with a number that is not representable when squared.
            inertia: Vec3::splat(PLAYER_MASS),
            damping: 0.0,
            rot_damping: 0.0,
        };
        let body = env.add(
            Motion::Player,
            &hulls,
            origin,
            Vec3::ZERO,
            // `Q_strncpy( solid.surfaceprop, "player", … )`.
            "player",
            Some(mass),
        )?;
        Some(PlayerController {
            body,
            bounds: (mins, maxs),
            max_speed: Vec3::ZERO,
            last_impulse: Vec3::ZERO,
            enabled: false,
            in_contact: false,
        })
    }

    /// The body, for a caller that needs to name it — a test, a console
    /// command, or [`Environment::remove`].
    ///
    /// Nothing in the running game asks: [`destroy`](PlayerController::destroy)
    /// is how the body is given back, and the sweep filter excludes
    /// [`Motion::Player`] without being told which one.
    #[allow(dead_code)]
    pub fn body(&self) -> BodyId {
        self.body
    }

    /// `IPhysicsPlayerController::IsInContact` — whether the last drive found
    /// the body touching something the *solver* moves, as opposed to the world
    /// or a game-driven mover.
    ///
    /// `CPlayerController::IsInContact` (`physics_shadow.cpp:742`) skips
    /// anything `physical_unmoveable` or `pinned`, and skips anything the game
    /// controls (`IsControlledByGame`) — which here is every kinematic body,
    /// i.e. every door.
    pub fn in_contact(&self) -> bool {
        self.in_contact
    }

    /// `CBasePlayer::SetVCollisionState`'s hull swap — call it every tick
    /// with the player's live bounds and it does nothing until they change.
    pub fn set_bounds(&mut self, env: &mut Environment, mins: Vec3, maxs: Vec3) {
        if (mins, maxs) == self.bounds {
            return;
        }
        self.bounds = (mins, maxs);
        env.set_hulls(self.body, &Hulls::from_box(mins, maxs), "player");
    }

    /// `PhysDestroyObject` for the shadow — called when the level ends or the
    /// player is removed.
    pub fn destroy(self, env: &mut Environment) {
        env.remove(self.body);
    }

    /// `CPlayerController::Update` followed by `do_simulation_controller`,
    /// which here are one call because this port's tick and its physics step
    /// are the same 1/64 s and adjacent.
    ///
    /// `target` is where `client/`'s movement put the player this tick and
    /// `velocity` is the velocity it wants — `g_pMoveData->m_outWishVel`,
    /// which is what `CBasePlayer::PostThinkVPhysics` stores and
    /// `UpdateVPhysicsPosition` passes on.
    ///
    /// Call it **before** [`Environment::step`]: it writes the body's velocity
    /// for the step that is about to happen, and reads the contacts the
    /// previous one left.
    ///
    /// # What the collapse of the two calls removes
    ///
    /// `m_secondsToArrival` and the `fraction = dt / secondsToArrival`
    /// resample exist because Valve's controller runs inside the solver and
    /// may run several times between two game updates, or none. Here it runs
    /// exactly once per update, so `secondsToArrival` is always `dt`,
    /// `fraction` is always 1 and `scaleDelta` is always `1 / dt`. The
    /// `!m_updatedSinceLast` branch — which caps a non-updated step to the
    /// last good impulse — becomes unreachable for the same reason; it is
    /// kept in [`last_impulse`](PlayerController::last_impulse) only because
    /// `GetLastImpulse` is what `CBasePlayer` reads to decide it has been
    /// crushed, and that is a class this port has not got.
    pub fn drive(&mut self, env: &mut Environment, target: Vec3, velocity: Vec3, dt: f32) {
        // `if ( velocity.LengthSqr() <= 0.1f ) { m_enable = false; }` — "no
        // input velocity, just go where physics takes you". **A standing
        // player does not push**, which is not an optimisation: it is what
        // stops the shadow grinding a cube across the floor while you stand
        // against it.
        self.enabled = velocity.length_squared() > 0.1;

        let Some((position, _)) = env.pose(self.body) else {
            return;
        };
        let delta = target - position;
        // `if ( m_forceTeleport || qdist > m_maxDeltaPosition² ) TryTeleportObject()`.
        // There is no `ShouldMoveTo` handler to consult — no Portal 2 class
        // installs one; see `portdocs/VPHYSICS_SHADOW.md` §3.5 — so the
        // teleport is unconditional and the step that would have driven it is
        // skipped, exactly as the `return` after `TryTeleportObject` does.
        //
        // > **It is tested before the enable check, where Valve tests it
        // > after**, and that is a consequence of §6.2 rather than a second
        // > opinion. Valve's shadow keeps its gravity and is held still by the
        // > floor, so a disabled one goes nowhere; this one is weightless, and
        // > a disabled one that had been shoved would drift away from the
        // > player for as long as they stood still with nothing to stop it and
        // > nothing to bring it back. The teleport is the only thing that can,
        // > so it has to run on both paths.
        if delta.length_squared() > TELEPORT_DISTANCE * TELEPORT_DISTANCE {
            env.teleport(self.body, target);
            env.set_velocity(self.body, Vec3::ZERO);
            self.last_impulse = Vec3::ZERO;
            self.in_contact = self.contacts_something_simulated(env);
            return;
        }
        if !self.enabled {
            // …and for the same reason, a disabled shadow is stopped rather
            // than left coasting. Valve's friction against the floor does this
            // for them.
            env.set_velocity(self.body, Vec3::ZERO);
            self.in_contact = self.contacts_something_simulated(env);
            return;
        }
        self.max_speed = max_speed(velocity, env.velocity(self.body));

        let mut speed = env.velocity(self.body);
        let impulse = compute_controller(&mut speed, delta, self.max_speed, 1.0 / dt, DAMP_FACTOR);
        self.last_impulse = impulse;

        // The contact clamp — `CNormalList` and everything around it.
        let contacts = env.contacts(self.body);
        self.in_contact = contacts
            .iter()
            .any(|contact| contact.moveable && contact.other.is_some());
        let clamped = clamp_against_contacts(speed, &contacts, PUSH_SPEED_LIMIT);
        self.last_impulse += clamped - speed;
        env.set_velocity(self.body, clamped);
    }

    fn contacts_something_simulated(&self, env: &Environment) -> bool {
        env.contacts(self.body)
            .iter()
            .any(|contact| contact.moveable && contact.other.is_some())
    }
}

/// `ComputeController`, the per-axis overload (`physics_shadow.cpp:94`) —
/// the whole of what a controller does.
///
/// ```text
/// acceleration  = delta * scale_delta
/// acceleration -= current * damping
/// clamp |acceleration[i]| to max_speed[i], per axis
/// current      += acceleration
/// ```
///
/// Returns the acceleration it applied, which is Valve's `pOutImpulse`.
///
/// > **The clamp is per axis and not on the magnitude.** Valve's other
/// > overload (`:46`, used by the *shadow* controller) clamps the length; this
/// > one walks `for(int i=2; i>=0; i--)`. The difference shows on a diagonal:
/// > a per-axis clamp lets the vector reach `√3` times the cap.
fn compute_controller(
    current: &mut Vec3,
    delta: Vec3,
    max_speed: Vec3,
    scale_delta: f32,
    damping: f32,
) -> Vec3 {
    // `if ( currentSpeed.quad_length() < 1e-6 ) currentSpeed.set_to_zero();`
    if current.length_squared() < 1e-6 {
        *current = Vec3::ZERO;
    }
    let mut acceleration = delta * scale_delta - *current * damping;
    for axis in 0..3 {
        let cap = max_speed[axis];
        if acceleration[axis].abs() >= cap {
            acceleration[axis] = match acceleration[axis] < 0.0 {
                true => -cap,
                false => cap,
            };
        }
    }
    *current += acceleration;
    acceleration
}

/// `CPlayerController::MaxSpeed` (`physics_shadow.cpp:699`) — how much the
/// controller may add this step, per axis.
///
/// The intent is "the part of the wish velocity that is not already being
/// delivered": project the current velocity onto the wish direction and
/// subtract that much of it, then take the result componentwise.
///
/// > **Valve's has one factor of speed too many** and this does not.
/// > `ivpVel.mult( dot * length )` multiplies the unit wish direction by the
/// > projection *and* by the wish speed, where only the projection belongs;
/// > `real_length_plus_normize` returns the former length
/// > (`ivu_linear.cxx:109`), so the units come out as speed² and the
/// > subtracted term overshoots by a factor of the wish speed. The result is
/// > then passed through `fabsf` per axis, so the error never shows as a
/// > negative number — it shows as a cap several times looser than intended
/// > whenever the player is already moving the way they are asking to.
/// > `portdocs/VPHYSICS_SHADOW.md` §6.1 is why this one is corrected where
/// > `Mass::inertia`'s bug is reproduced.
fn max_speed(wish: Vec3, current: Vec3) -> Vec3 {
    let Some(direction) = wish.try_normalize() else {
        return Vec3::ZERO;
    };
    let dot = direction.dot(current);
    let available = match dot > 0.0 {
        true => wish - direction * dot,
        false => wish,
    };
    available.abs()
}

/// `CNormalList` (`physics_shadow.cpp:420`) plus the loop that fills it
/// (`:571-611`) — what the shadow is allowed to do to what it is touching.
///
/// Two gates decide whether a contact's normal is collected at all:
///
/// - **A contact with something immovable, or heavier than
///   [`PUSH_MASS_LIMIT`], drops the limit to zero** for every contact,
///   including ones already seen. Valve writes it as a running `limitVel` and
///   so does this; the order dependence is real and is Valve's.
/// - **A contact whose normal is steeper than `-0.99` is skipped entirely** —
///   the floor — under a comment reading `// remove this when clamp works
///   better`. Without it, standing still would be clamped to a stop against
///   the ground on every step.
///
/// Then `ClampVector`: one normal clips the component along it, two project
/// onto their crease, three or more stop the motion dead.
fn clamp_against_contacts(
    velocity: Vec3,
    contacts: &[super::env::Contact],
    speed_limit: f32,
) -> Vec3 {
    let mut limit = speed_limit;
    let mut normals: Vec<Vec3> = Vec::new();
    for contact in contacts {
        // `if ( normal.z > -0.99f )`, which is the *floor* being excluded —
        // the normal points from the player towards what it is touching, so
        // the ground's is `z ≈ -1`. See `Contact::normal`.
        if contact.normal.z <= -0.99 {
            continue;
        }
        if !contact.moveable || contact.mass.is_none_or(|mass| mass > PUSH_MASS_LIMIT) {
            limit = 0.0;
        }
        let push_speed = velocity.dot(contact.normal);
        // `contactVel = pSnapshot->GetNormalForce() * invMass`, the velocity
        // the contact is already imparting. `invMass` is the *player's*.
        let contact_vel = contact.normal_force / PLAYER_MASS;
        if push_speed + contact_vel <= limit {
            continue;
        }
        // `CNormalList::AddNormal` — full at eight, and a normal within
        // `dot > 0.99` of one already held is the same plane twice.
        if normals.len() >= MAX_NORMALS {
            continue;
        }
        if normals.iter().any(|held| held.dot(contact.normal) > 0.99) {
            continue;
        }
        normals.push(contact.normal);
    }
    clamp_vector(velocity, &normals, limit)
}

/// `CNormalList::ClampVector` (`physics_shadow.cpp:450`).
fn clamp_vector(velocity: Vec3, normals: &[Vec3], limit: f32) -> Vec3 {
    match normals.len() {
        0 => velocity,
        1 => {
            let dot = velocity.dot(normals[0]);
            match dot > limit {
                true => velocity + normals[0] * (limit - dot),
                false => velocity,
            }
        }
        2 => {
            // The crease the two planes make, and however much of the motion
            // runs along it. Not normalised, and that is Valve's:
            // `crease * dot` where `crease` is a raw cross product, so two
            // nearly-parallel normals shrink the result towards zero.
            let crease = normals[0].cross(normals[1]);
            crease * velocity.dot(crease)
        }
        _ => match normals.iter().any(|normal| velocity.dot(*normal) > 0.0) {
            true => Vec3::ZERO,
            false => velocity,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vphysics::collide::{Ledge, Solid, SolidParams};
    use crate::vphysics::env::TIMESTEP;
    use crate::vphysics::surfaceprops::SurfaceProps;

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

    /// A 4,096-unit floor at z = 0.
    fn floor(env: &mut Environment) {
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
        env.add(
            Motion::Static,
            &Hulls::from_solid(&solid),
            Vec3::ZERO,
            Vec3::ZERO,
            "default",
            None,
        )
        .expect("a floor");
    }

    /// A 40 kg cube, the shipped `metal_box`'s mass.
    fn cube(env: &mut Environment, origin: Vec3) -> BodyId {
        let solid = cube_solid(16.0);
        let params = SolidParams {
            mass: 40.0,
            ..Default::default()
        };
        env.add(
            Motion::Dynamic,
            &Hulls::from_solid(&solid),
            origin,
            Vec3::ZERO,
            "default",
            Some(Mass::from_solid(&solid, &params)),
        )
        .expect("a cube")
    }

    fn player(env: &mut Environment, origin: Vec3) -> PlayerController {
        PlayerController::new(
            env,
            origin,
            Vec3::new(-16.0, -16.0, 0.0),
            Vec3::new(16.0, 16.0, 72.0),
        )
        .expect("a shadow")
    }

    /// With `damping` at 1 the controller writes exactly the velocity that
    /// closes the gap in one step, whatever the body was doing before.
    #[test]
    fn the_controller_closes_the_gap_in_one_step() {
        let dt = 1.0 / 64.0;
        let mut speed = Vec3::new(-30.0, 7.0, 0.0);
        let delta = Vec3::new(2.0, 0.0, 0.0);
        compute_controller(&mut speed, delta, Vec3::splat(1e9), 1.0 / dt, DAMP_FACTOR);
        assert!(
            (speed - delta / dt).length() < 1e-2,
            "expected {:?}, got {speed:?}",
            delta / dt
        );
    }

    /// **The clamp is per axis**, so a diagonal reaches √3 times the cap —
    /// Valve's `for(int i=2; i>=0; i--)`, not a length clamp.
    #[test]
    fn the_clamp_is_per_axis_so_a_diagonal_reaches_root_three() {
        let mut speed = Vec3::ZERO;
        compute_controller(&mut speed, Vec3::splat(1000.0), Vec3::splat(10.0), 1.0, 0.0);
        assert_eq!(speed, Vec3::splat(10.0));
        assert!((speed.length() - 10.0 * 3f32.sqrt()).abs() < 1e-3);
    }

    /// A player already moving at exactly the velocity they are asking for has
    /// nothing left to spend, so the cap is zero and the shadow adds nothing.
    ///
    /// This is the property Valve's extra `* length` destroys — see
    /// [`max_speed`].
    #[test]
    fn nothing_is_available_when_the_wish_is_already_being_delivered() {
        let wish = Vec3::new(175.0, 0.0, 0.0);
        assert_eq!(max_speed(wish, wish), Vec3::ZERO);
        // Half delivered, half left.
        assert_eq!(max_speed(wish, wish * 0.5), Vec3::new(87.5, 0.0, 0.0));
        // Moving the other way spends nothing: the `dot > 0` gate.
        assert_eq!(max_speed(wish, -wish), wish);
    }

    /// `ClampVector`'s three cases.
    #[test]
    fn a_clamped_vector_slides_creases_and_stops() {
        let east = Vec3::X;
        let north = Vec3::Y;
        let motion = Vec3::new(100.0, 100.0, 0.0);

        // Nothing in the way.
        assert_eq!(clamp_vector(motion, &[], 0.0), motion);
        // One plane: the component along it is clipped to the limit.
        assert_eq!(clamp_vector(motion, &[east], 0.0), Vec3::new(0.0, 100.0, 0.0));
        assert_eq!(
            clamp_vector(motion, &[east], 50.0),
            Vec3::new(50.0, 100.0, 0.0)
        );
        // Two planes: whatever runs along their crease, which here is
        // vertical and the motion is not.
        assert_eq!(clamp_vector(motion, &[east, north], 0.0), Vec3::ZERO);
        // Three planes with any positive projection: dead stop.
        assert_eq!(
            clamp_vector(motion, &[east, north, Vec3::Z], 0.0),
            Vec3::ZERO
        );
    }

    /// **A standing player does not push.** `Update`'s
    /// `if ( velocity.LengthSqr() <= 0.1f ) m_enable = false`, which is what
    /// stops the shadow grinding a cube across the floor while you lean on it.
    #[test]
    fn a_standing_player_does_not_push() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let mut shadow = player(&mut env, Vec3::ZERO);
        let cube = cube(&mut env, Vec3::new(40.0, 0.0, 16.0));

        // Half a second of walking east, to get the shadow up against the
        // cube. Asserting on a player who never touched it would prove
        // nothing.
        let mut target = Vec3::ZERO;
        for _ in 0..32 {
            target.x += 175.0 * TIMESTEP;
            shadow.drive(&mut env, target, Vec3::new(175.0, 0.0, 0.0), TIMESTEP);
            env.step();
        }
        let leaning = env.pose(cube).expect("a cube").0;
        let pushed = env.velocity(cube).x;
        assert!(leaning.x > 40.0, "the walk should have moved it at all");
        assert!(pushed > 1.0, "…and given it some speed, not {pushed}");

        // Now stand still against it for a second. `m_enable` goes false, the
        // controller writes nothing, and what is left is a cube coasting to a
        // stop on its own friction.
        //
        // **The test is the deceleration, not the distance.** A cube that has
        // been shoved carries its momentum for as long as the floor lets it,
        // so "did it stop moving" would be a test of `surfaceprops` and not of
        // this; "is it still being accelerated" is the question `m_enable`
        // answers.
        for _ in 0..64 {
            shadow.drive(&mut env, target, Vec3::ZERO, TIMESTEP);
            env.step();
        }
        let coasting = env.velocity(cube).x;
        assert!(
            coasting < pushed,
            "a standing player kept accelerating the cube: {pushed} -> {coasting}"
        );
    }

    /// The whole point: walk into a cube and it moves.
    #[test]
    fn a_walking_player_shoves_a_cube() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let mut shadow = player(&mut env, Vec3::ZERO);
        let cube = cube(&mut env, Vec3::new(40.0, 0.0, 16.0));
        let before = env.pose(cube).expect("a cube").0;

        // A second of walking east at 175 u/s, with the player's own position
        // advancing the way the movement code would advance it. The cube is in
        // the way, and the player walks *through* where it is — which is what
        // the movement code would not do, and is exactly the case the
        // controller exists to absorb.
        let wish = Vec3::new(175.0, 0.0, 0.0);
        let mut target = Vec3::ZERO;
        for _ in 0..64 {
            target.x += 175.0 * TIMESTEP;
            shadow.drive(&mut env, target, wish, TIMESTEP);
            env.step();
        }
        let after = env.pose(cube).expect("still a cube").0;
        assert!(
            after.x - before.x > 8.0,
            "the cube should have been shoved east, went {before:?} -> {after:?}"
        );
        assert!(
            after.z > 8.0,
            "…along the floor, not through it: {after:?}"
        );
    }

    /// **A disabled shadow is still recovered**, which is this port's and not
    /// Valve's — see `drive`. A weightless body that had been shoved would
    /// otherwise drift for as long as the player stood still.
    #[test]
    fn a_standing_players_shadow_is_stopped_rather_than_left_coasting() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let mut shadow = player(&mut env, Vec3::ZERO);

        // Shove the body sideways behind the controller's back, the way a
        // cube rebounding off it would.
        env.set_velocity(shadow.body(), Vec3::new(400.0, 0.0, 0.0));
        shadow.drive(&mut env, Vec3::ZERO, Vec3::ZERO, TIMESTEP);
        assert_eq!(
            env.velocity(shadow.body()),
            Vec3::ZERO,
            "a standing player's shadow does not coast"
        );

        // And if it got away before anyone looked, the teleport brings it
        // back even though the controller is disabled.
        env.teleport(shadow.body(), Vec3::new(500.0, 0.0, 0.0));
        shadow.drive(&mut env, Vec3::ZERO, Vec3::ZERO, TIMESTEP);
        let (origin, _) = env.pose(shadow.body()).expect("a shadow");
        assert!(
            origin.length() < 0.01,
            "expected a teleport back to the player, ended at {origin:?}"
        );
    }

    /// A shadow left more than [`TELEPORT_DISTANCE`] behind is put back rather
    /// than driven back — `m_maxDeltaPosition`, and the reason the body may be
    /// wrong without the player noticing.
    #[test]
    fn a_shadow_left_too_far_behind_is_teleported() {
        let mut env = Environment::new(SurfaceProps::default());
        floor(&mut env);
        let mut shadow = player(&mut env, Vec3::ZERO);

        let far = Vec3::new(1000.0, 0.0, 0.0);
        shadow.drive(&mut env, far, Vec3::new(175.0, 0.0, 0.0), TIMESTEP);
        let (origin, _) = env.pose(shadow.body()).expect("a shadow");
        assert!(
            (origin - far).length() < 0.01,
            "expected a teleport to {far:?}, ended at {origin:?}"
        );

        // …and a drift inside the limit is *not* a teleport: the body is still
        // where it was and the controller drives it from there.
        let near = Vec3::new(10.0, 0.0, 0.0) + far;
        shadow.drive(&mut env, near, Vec3::new(175.0, 0.0, 0.0), TIMESTEP);
        let (origin, _) = env.pose(shadow.body()).expect("a shadow");
        assert!(
            (origin - near).length() > 1.0,
            "a 10-unit gap should be driven, not teleported"
        );
    }

    /// The push limits: a contact with something immovable forbids the push
    /// entirely, which is the branch the *world* takes on every step.
    #[test]
    fn an_immovable_contact_forbids_the_push() {
        let wall = super::super::env::Contact {
            normal: Vec3::X,
            other: None,
            moveable: false,
            mass: None,
            normal_force: 0.0,
        };
        let motion = Vec3::new(100.0, 0.0, 0.0);
        assert_eq!(
            clamp_against_contacts(motion, &[wall], PUSH_SPEED_LIMIT),
            Vec3::ZERO,
            "nothing may be pushed into an immovable surface"
        );
    }

    /// …and a light prop is pushed, but only at [`PUSH_SPEED_LIMIT`].
    #[test]
    fn a_light_prop_is_pushed_at_the_speed_limit_and_no_faster() {
        let cube = super::super::env::Contact {
            normal: Vec3::X,
            other: None,
            moveable: true,
            mass: Some(40.0),
            normal_force: 0.0,
        };
        let motion = Vec3::new(175.0, 0.0, 0.0);
        let clamped = clamp_against_contacts(motion, &[cube], PUSH_SPEED_LIMIT);
        assert!(
            (clamped.x - PUSH_SPEED_LIMIT).abs() < 1e-3,
            "expected {PUSH_SPEED_LIMIT}, got {clamped:?}"
        );
    }

    /// **The floor is skipped**, `normal.z > -0.99`, or standing still would
    /// be clamped to a stop against the ground on every step.
    #[test]
    fn the_ground_is_not_a_plane_the_push_is_clamped_against() {
        let ground = super::super::env::Contact {
            normal: -Vec3::Z,
            other: None,
            moveable: false,
            mass: None,
            normal_force: 800.0,
        };
        let motion = Vec3::new(175.0, 0.0, -10.0);
        assert_eq!(
            clamp_against_contacts(motion, &[ground], PUSH_SPEED_LIMIT),
            motion,
            "the floor must not clamp anything"
        );
    }
}
