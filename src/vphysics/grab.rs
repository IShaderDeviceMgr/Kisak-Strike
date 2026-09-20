//! Carrying a physics prop — `CGrabController`, from
//! `legacy/game/shared/portal2/portal_grabcontroller_shared.cpp`.
//!
//! `portdocs/VPHYSICS_GRAB.md` is the design. This is the half that drives a
//! *body*; [`crate::server::grab`] is the half that knows which entity it is
//! and where the player wants it.
//!
//! # It is the shadow controller again, with the angular half turned on
//!
//! [`shadow`](super::shadow) drives the player's own body towards a point.
//! This drives a *held* body towards a point **and an orientation**, using the
//! same `ComputeShadowControllerIVP` (`physics_shadow.cpp:826`) that
//! `IPhysicsObject::ComputeShadowControl` is a thin wrapper over.
//!
//! > **`ComputeController` has two overloads and they are not
//! > interchangeable.** `physics_shadow.cpp:46` clamps the acceleration by
//! > vector *magnitude* against a scalar limit and keeps damping as a second,
//! > separately clamped term; `physics_shadow.cpp:94` clamps **per axis** and
//! > folds damping into the acceleration. The shadow control calls the first,
//! > `CPlayerController` calls the second, and
//! > [`shadow::compute_controller`](super::shadow) is the second. Reusing it
//! > here would clamp a diagonal carry to `maxSpeed` on each axis — `√3` times
//! > the intended speed — and would drop `maxDampSpeed` entirely. Hence
//! > [`compute_controller`] below, which is the *first*.
//!
//! # Why the held object weighs one kilogram
//!
//! [`CARRY_MASS`] is `REDUCED_CARRY_MASS`
//! (`portal_grabcontroller_shared.cpp:278`), and it is `1.0`. A 40 kg cube
//! held fifteen units in front of an 85 kg player is a lever; at 1 kg it is
//! not. The original mass is saved on attach and put back on detach, and
//! nothing else in the port may write a held body's mass while it is held.

use glam::{Quat, Vec3};

use super::env::{BodyId, Contact, Environment};

/// `REDUCED_CARRY_MASS` (`portal_grabcontroller_shared.cpp:278`), in
/// kilograms.
pub const CARRY_MASS: f32 = 1.0;

/// `m_shadow.maxSpeed`, set in `CGrabController::CGrabController` (`:281`).
///
/// > **`ComputeMaxSpeed` (`:530`) is dead code for everything the player can
/// > lift**, so it is not ported. It returns this unchanged unless the object
/// > outweighs `physcannon_maxmass` (250 kg), and `CanPickupObject` refuses
/// > anything over 85. `portdocs/VPHYSICS_GRAB.md` §6.3 keeps the arithmetic
/// > it collapses to.
pub const MAX_SPEED: f32 = 1000.0;

/// `DEFAULT_MAX_ANGULAR` (`:277`) — `360 * 10` **degrees** per second.
///
/// Stored here in Valve's units and converted where it is used, because the
/// shipped constant is the recognisable number and the radians are an artefact
/// of the solver underneath.
pub const MAX_ANGULAR_DEGREES: f32 = 3600.0;

/// `m_shadow.dampFactor`.
const DAMP_FACTOR: f32 = 1.0;

/// The rotational damping `AttachEntity` (`:796`) puts on a held object, so
/// that it stops tumbling in the player's hands.
const CARRY_ANGULAR_DAMPING: f32 = 10.0;

/// `m_errorTime = -1.0f` (`:809`) — one second of grace after a grab before
/// error accumulates at all, so that snatching a cube off a shelf does not
/// immediately drop it.
const ERROR_GRACE: f32 = -1.0;

/// What `CPlayerPickupController::UsePickupController` (`:1356`) drops the
/// object at.
///
/// Valve's expression is
/// `player_held_object_collide_with_player ? 12 : 40`, and that cvar defaults
/// to `0`, so **40** is the shipped number. The comment beside it is the
/// reason it is so large: *"what we want to allow is the object moving from
/// the desired position into the player before we start trying to break the
/// hold"*.
///
/// > **The other threshold in that function is unreachable.**
/// > `UsePickupController` calls `ComputeError()` and then calls
/// > `UpdateObject( player, 12 )`, which calls `ComputeError()` again — but
/// > `ComputeError` ends by setting `m_errorTime = 0` and begins with
/// > `if ( m_errorTime <= 0 ) return 0`, so the second call is structurally
/// > zero and the `12` can never fire. `portdocs/VPHYSICS_GRAB.md` §6.1.
pub const MAX_ERROR: f32 = 40.0;

/// How fast `m_contactAmount` moves, per second — `deltaTime * 2.0f` in
/// `Simulate` (`:1023`).
const CONTACT_RATE: f32 = 2.0;

/// What `m_contactAmount` approaches while the object is jammed against
/// something heavy.
const CONTACT_JAMMED: f32 = 0.1;

/// One held object. `CGrabController`, minus the view-model half.
#[derive(Debug, Clone)]
pub struct GrabController {
    body: BodyId,
    /// `m_savedMass[0]` — what the object weighed before [`CARRY_MASS`].
    saved_mass: f32,
    /// `m_savedRotDamping[0]`.
    saved_angular_damping: f32,
    /// `m_flLoadWeight` — the real mass, which is what decides whether a
    /// contact counts as "heavy" in [`slide`].
    load_weight: f32,
    /// `m_shadow.targetPosition` / `targetRotation`, as last set by
    /// [`drive`](GrabController::drive).
    target: Vec3,
    target_rotation: Quat,
    /// `m_error` and `m_errorTime`. See [`MAX_ERROR`].
    error: f32,
    error_time: f32,
    /// `m_contactAmount`, which cubes the angular limit while the object is
    /// jammed.
    contact_amount: f32,
}

impl GrabController {
    /// `CGrabController::AttachEntity` (`:609`), minus everything that is
    /// about an *entity*.
    ///
    /// Saves the mass and rotational damping, replaces them, wakes the body
    /// and marks it held — which is both a trace filter and a collision
    /// filter, see [`Environment::set_held`].
    pub fn attach(env: &mut Environment, body: BodyId) -> GrabController {
        let saved_mass = env.mass(body).unwrap_or(CARRY_MASS);
        let saved_angular_damping = env.angular_damping(body);
        let (target, target_rotation) = match (env.pose(body), env.rotation(body)) {
            (Some((origin, _)), Some(rotation)) => (origin, rotation),
            _ => (Vec3::ZERO, Quat::IDENTITY),
        };
        env.set_mass(body, CARRY_MASS);
        env.set_angular_damping(body, CARRY_ANGULAR_DAMPING);
        env.set_held(body, true);
        env.wake(body);
        GrabController {
            body,
            saved_mass,
            saved_angular_damping,
            load_weight: saved_mass,
            target,
            target_rotation,
            error: 0.0,
            error_time: ERROR_GRACE,
            contact_amount: 0.0,
        }
    }

    pub fn body(&self) -> BodyId {
        self.body
    }

    /// `CGrabController::DetachEntity` (`:879`) — put the mass back and let
    /// go.
    ///
    /// `player_velocity` and `max_speed` are the holder's, because the
    /// outgoing velocity is clamped **relative to the player**
    /// (`ClampPhysicsVelocity`, `:860`): a cube let go at a run keeps the
    /// player's own motion and gains at most half as much again on top, which
    /// is what stops a drop from becoming a throw.
    pub fn detach(self, env: &mut Environment, player_velocity: Vec3, max_speed: f32) {
        env.set_mass(self.body, self.saved_mass);
        env.set_angular_damping(self.body, self.saved_angular_damping);
        env.set_held(self.body, false);
        let relative = env.velocity(self.body) - player_velocity;
        let clamped = clamp_length(relative, max_speed * 1.5) + player_velocity;
        env.set_velocity(self.body, clamped);
        let angular = clamp_length(
            env.angular_velocity(self.body),
            (2.0 * 360.0f32).to_radians(),
        );
        env.set_angular_velocity(self.body, angular);
        env.wake(self.body);
    }

    /// `CGrabController::Simulate` (`:1012`) and `SetTargetPosition` (`:325`)
    /// together: aim the object and run one step of the shadow control.
    ///
    /// **Call it before [`Environment::step`]**, once per tick — it writes the
    /// velocities the step is about to integrate, exactly as
    /// [`PlayerController::drive`](super::shadow::PlayerController::drive)
    /// does.
    ///
    /// `player_speed` is `m_fPlayerSpeed`, added to the speed limit so that a
    /// running player does not outrun what they are carrying (`:1021`).
    pub fn drive(
        &mut self,
        env: &mut Environment,
        target: Vec3,
        rotation: Quat,
        player_speed: f32,
        dt: f32,
    ) {
        if dt <= 0.0 {
            return;
        }
        self.target = target;
        self.target_rotation = rotation;
        // `SetTargetPosition`'s `pObj->Wake()` — a held object that has gone
        // to sleep stops answering the controller entirely.
        env.wake(self.body);

        let contacts = env.contacts(self.body);
        // `InContactWithHeavyObject( pObject, GetLoadWeight() )` (`:994`),
        // which is the object's *real* mass and not the 1 kg it is carrying —
        // so "heavier than me" means what it meant before the grab.
        let jammed = contacts
            .iter()
            .any(|c| !c.moveable || c.mass.is_some_and(|mass| mass > self.load_weight));
        let goal = match jammed {
            true => CONTACT_JAMMED,
            false => 1.0,
        };
        self.contact_amount = approach(goal, self.contact_amount, dt * CONTACT_RATE);

        // `m_timeToArrive` is `UTIL_GetSimulationInterval()`, set fresh by
        // every `SetTargetPosition`, so `fraction` is `dt / dt` — one. The
        // resampling exists for callers that aim several ticks ahead and this
        // is not one of them; the arithmetic is kept so the shape matches the
        // reference, and collapses.
        let scale_delta = 1.0 / dt;
        let max_speed = MAX_SPEED + player_speed;

        let mut velocity = env.velocity(self.body);
        compute_controller(
            &mut velocity,
            self.target - env.pose(self.body).map_or(Vec3::ZERO, |(o, _)| o),
            max_speed,
            max_speed * 2.0,
            scale_delta,
            DAMP_FACTOR,
        );

        // `shadowParams.maxAngular = m_shadow.maxAngular * m_contactAmount³`
        // — the "stop spinning while jammed" fix, and the cube is why it
        // exists.
        let max_angular = MAX_ANGULAR_DEGREES.to_radians() * self.contact_amount.powi(3);
        let mut angular = env.angular_velocity(self.body);
        let delta = env
            .rotation(self.body)
            .map_or(Vec3::ZERO, |current| angular_delta(self.target_rotation, current));
        compute_controller(
            &mut angular,
            delta,
            max_angular,
            max_angular,
            scale_delta,
            DAMP_FACTOR,
        );

        // `PhysComputeSlideDirection` (`physics_shared.cpp:826`) — slide along
        // whatever the object is resting against instead of driving into it,
        // which is what stops a carried cube bouncing down a corridor wall.
        let (velocity, angular) = slide(velocity, angular, &contacts, self.load_weight);
        env.set_velocity(self.body, velocity);
        env.set_angular_velocity(self.body, angular);
        self.error_time += dt;
    }

    /// `CGrabController::ComputeError` (`:376`), minus the portal and
    /// obstruction multipliers.
    ///
    /// **It has a side effect and the side effect is the point**: it consumes
    /// `m_errorTime`, so a second call in the same tick returns zero. See
    /// [`MAX_ERROR`].
    pub fn error(&mut self, env: &Environment) -> f32 {
        if self.error_time <= 0.0 {
            return 0.0;
        }
        let position = env.pose(self.body).map_or(self.target, |(o, _)| o);
        let mut error = (self.target - position).length();
        let weight = self.error_time.min(1.0);
        // `if ( speed > m_shadow.maxSpeed ) error *= 0.5` — an object that is
        // behind because it is being driven *fast* is not an object that has
        // been left behind.
        if error / weight > MAX_SPEED {
            error *= 0.5;
        }
        self.error = (1.0 - weight) * self.error + error * weight;
        self.error_time = 0.0;
        self.error
    }

    /// `m_flLoadWeight` — what the object really weighs, not the 1 kg it is
    /// carrying while held.
    ///
    /// `#[allow(dead_code)]` for the same reason
    /// [`Environment::pose`](super::env::Environment::pose) carries one: the
    /// value is used inside [`drive`](GrabController::drive) on every tick,
    /// and the *accessor* exists so that a test can pin the invariant that the
    /// real mass survives being replaced by [`CARRY_MASS`].
    #[allow(dead_code)]
    pub fn load_weight(&self) -> f32 {
        self.load_weight
    }
}

/// `ComputeController`, **the scalar overload** (`physics_shadow.cpp:46`).
///
/// Clamps the correcting acceleration by vector magnitude, and damps with a
/// second term clamped by its own limit. See this module's header for why the
/// per-axis overload in [`shadow`](super::shadow) is not this.
fn compute_controller(
    current: &mut Vec3,
    delta: Vec3,
    max_speed: f32,
    max_damp_speed: f32,
    scale_delta: f32,
    damping: f32,
) {
    if current.length_squared() < 1e-6 {
        *current = Vec3::ZERO;
    }
    let acceleration = clamp_length(delta * scale_delta, max_speed);
    let damp = clamp_length(*current * -damping, max_damp_speed);
    *current += damp + acceleration;
}

/// `VectorNormalize` then a scalar clamp — Valve's
/// `speed = maxSpeed / speed; v.mult( speed )`, with `maxSpeed <= 0` meaning
/// "no motion at all" rather than "no limit".
fn clamp_length(v: Vec3, max: f32) -> Vec3 {
    if max <= 0.0 {
        return Vec3::ZERO;
    }
    let length = v.length();
    match length > max {
        true => v * (max / length),
        false => v,
    }
}

/// The angular error between where a body is and where it is told to be, as an
/// axis-angle vector in **world** space and in radians.
///
/// `QuaternionDiff` + `QuaternionAxisAngle`, both of which
/// `physics_shadow.cpp` defines for itself (`:784` and `:794`) rather than
/// taking `mathlib`'s — and the local copies differ from `mathlib`'s in two
/// ways that both matter:
///
/// > **`physics_shadow.cpp`'s `QuaternionAxisAngle` returns radians**, where
/// > `mathlib_base.cpp:2447`'s returns degrees. Reading the wrong one gives an
/// > angular error 57 times too large.
///
/// > **And the frame is different.** Valve's `QuaternionDiff( p, q )` is
/// > `q⁻¹ · p`, a delta in the body's *own* frame, because IVP keeps
/// > `IVP_Core::rot_speed` in core space. Rapier's `angvel` is in **world**
/// > space, so the same delta is `p · q⁻¹` — the other order. Transliterating
/// > the multiply would rotate the correction by the object's own orientation
/// > and make a tilted cube chase its tail.
fn angular_delta(target: Quat, current: Quat) -> Vec3 {
    let delta = (target * current.inverse()).normalize();
    // `angle = 2 * acos( q.w ); if ( angle > M_PI ) angle -= 2 * M_PI;` — the
    // shortest arc, which is what keeps a 359° correction from being taken the
    // long way round.
    let mut angle = 2.0 * delta.w.clamp(-1.0, 1.0).acos();
    if angle > std::f32::consts::PI {
        angle -= 2.0 * std::f32::consts::PI;
    }
    match Vec3::new(delta.x, delta.y, delta.z).try_normalize() {
        Some(axis) => axis * angle,
        // An identity rotation has no axis, which is not an error: there is
        // nothing to correct.
        None => Vec3::ZERO,
    }
}

/// `PhysComputeSlideDirection` (`physics_shared.cpp:826`).
///
/// For every contact against something immovable or heavier than the object
/// itself: project the velocity out of the contact normal if it is moving
/// *into* it, and reduce the spin to its component about that normal. Valve's
/// own comment on the second half is `BUGBUG: Figure out the correct rotation
/// clipping equation`, and the port keeps what ships.
fn slide(
    velocity: Vec3,
    angular: Vec3,
    contacts: &[Contact],
    min_mass: f32,
) -> (Vec3, Vec3) {
    let (mut velocity, mut angular) = (velocity, angular);
    for contact in contacts {
        if contact.moveable && contact.mass.is_none_or(|mass| mass <= min_mass) {
            continue;
        }
        angular = contact.normal * angular.dot(contact.normal);
        // "NOTE: Normal points away from this object" — the same convention
        // [`Contact::normal`] documents, so a positive projection is motion
        // *into* what is being touched.
        let projection = velocity.dot(contact.normal);
        if projection > 0.0 {
            velocity -= contact.normal * projection;
        }
    }
    (velocity, angular)
}

/// `Approach( target, value, speed )` (`mathlib`), which moves `value`
/// towards `target` by at most `speed` and does not overshoot.
fn approach(target: f32, value: f32, speed: f32) -> f32 {
    let delta = target - value;
    match delta > speed {
        true => value + speed,
        false => match delta < -speed {
            true => value - speed,
            false => target,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vphysics::collide::{Ledge, Solid, SolidParams};
    use crate::vphysics::env::{Hulls, Mass, Motion, TIMESTEP};
    use crate::vphysics::surfaceprops::SurfaceProps;

    fn box_solid(half: f32) -> Solid {
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

    /// A 40 kg cube, the shipped `metal_box`'s mass.
    fn cube(env: &mut Environment, origin: Vec3) -> BodyId {
        let solid = box_solid(16.0);
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

    fn env() -> Environment {
        Environment::new(SurfaceProps::default())
    }

    #[test]
    fn attaching_drops_the_mass_to_one_kilogram_and_detaching_puts_it_back() {
        let mut env = env();
        let body = cube(&mut env, Vec3::ZERO);
        assert_eq!(env.mass(body), Some(40.0));

        let grab = GrabController::attach(&mut env, body);
        assert_eq!(env.mass(body), Some(CARRY_MASS));
        // The *real* mass is what decides which contacts count as heavy, so
        // it has to survive the substitution.
        assert_eq!(grab.load_weight(), 40.0);

        grab.detach(&mut env, Vec3::ZERO, 175.0);
        assert_eq!(env.mass(body), Some(40.0));
    }

    /// The point of the whole module: a held object goes where it is told.
    #[test]
    fn a_held_cube_is_driven_to_where_it_is_told_to_be() {
        let mut env = env();
        let body = cube(&mut env, Vec3::ZERO);
        let mut grab = GrabController::attach(&mut env, body);

        let target = Vec3::new(64.0, 0.0, 48.0);
        for _ in 0..64 {
            grab.drive(&mut env, target, Quat::IDENTITY, 0.0, TIMESTEP);
            env.step();
        }
        let (origin, _) = env.pose(body).expect("the cube");
        assert!(
            origin.distance(target) < 1.0,
            "a held cube should arrive at its target, got {origin} for {target}"
        );
    }

    /// Gravity is not switched off for a held object — the controller simply
    /// out-drives it, which is what `maxSpeed = 1000` is for.
    #[test]
    fn a_held_cube_does_not_fall() {
        let mut env = env();
        let body = cube(&mut env, Vec3::new(0.0, 0.0, 128.0));
        let mut grab = GrabController::attach(&mut env, body);

        let target = Vec3::new(0.0, 0.0, 128.0);
        for _ in 0..128 {
            grab.drive(&mut env, target, Quat::IDENTITY, 0.0, TIMESTEP);
            env.step();
        }
        let (origin, _) = env.pose(body).expect("the cube");
        assert!(
            (origin.z - 128.0).abs() < 1.0,
            "a held cube should hold its height against gravity, got z = {}",
            origin.z
        );
    }

    #[test]
    fn a_held_cube_is_turned_to_the_orientation_it_is_given() {
        let mut env = env();
        let body = cube(&mut env, Vec3::ZERO);
        let mut grab = GrabController::attach(&mut env, body);

        let rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        for _ in 0..64 {
            grab.drive(&mut env, Vec3::ZERO, rotation, 0.0, TIMESTEP);
            env.step();
        }
        let actual = env.rotation(body).expect("the cube");
        // `angle_between` is the shortest arc, which is the same measure the
        // controller is correcting along.
        assert!(
            actual.angle_between(rotation) < 0.05,
            "a held cube should take the orientation it is given, off by {} rad",
            actual.angle_between(rotation)
        );
    }

    /// The grace period: `m_errorTime` starts at -1, so the first second of a
    /// hold reports no error however far behind the object is.
    #[test]
    fn error_is_zero_for_the_first_second_of_a_hold() {
        let mut env = env();
        let body = cube(&mut env, Vec3::ZERO);
        let mut grab = GrabController::attach(&mut env, body);
        assert_eq!(grab.error(&env), 0.0);

        // Aim a long way off and step for half a second — still inside the
        // grace, so still no error.
        for _ in 0..32 {
            grab.drive(&mut env, Vec3::new(4096.0, 0.0, 0.0), Quat::IDENTITY, 0.0, TIMESTEP);
            env.step();
        }
        assert_eq!(grab.error(&env), 0.0);
    }

    /// And once the grace is spent, an object that cannot reach its target
    /// reports an error that would break the hold.
    #[test]
    fn error_grows_once_the_grace_is_spent() {
        let mut env = env();
        let body = cube(&mut env, Vec3::ZERO);
        let mut grab = GrabController::attach(&mut env, body);
        // Two seconds of driving at a target the cube is nowhere near. The
        // controller is stiff, so the target has to be *moved* each tick to
        // stay ahead of it — which is what a player running away does.
        for tick in 0..128 {
            let target = Vec3::new(4096.0 + tick as f32 * 512.0, 0.0, 0.0);
            grab.drive(&mut env, target, Quat::IDENTITY, 0.0, TIMESTEP);
            env.step();
        }
        assert!(
            grab.error(&env) > MAX_ERROR,
            "a cube left far behind should break the hold, error was {}",
            grab.error(&env)
        );
    }

    /// `ComputeError` consumes `m_errorTime`, so the second call in a tick is
    /// zero — `portdocs/VPHYSICS_GRAB.md` §6.1, and the reason Valve's `12`
    /// threshold is unreachable.
    #[test]
    fn a_second_error_query_in_one_tick_reads_zero() {
        let mut env = env();
        let body = cube(&mut env, Vec3::ZERO);
        let mut grab = GrabController::attach(&mut env, body);
        for tick in 0..128 {
            let target = Vec3::new(4096.0 + tick as f32 * 512.0, 0.0, 0.0);
            grab.drive(&mut env, target, Quat::IDENTITY, 0.0, TIMESTEP);
            env.step();
        }
        assert!(grab.error(&env) > 0.0);
        assert_eq!(grab.error(&env), 0.0);
    }

    /// A held body must not stop the player's own movement trace, or the
    /// player could not walk forwards while carrying anything.
    #[test]
    fn a_held_cube_is_invisible_to_a_sweep() {
        let mut env = env();
        let body = cube(&mut env, Vec3::new(64.0, 0.0, 0.0));
        // One step so the broad phase has the cube in it — Rapier builds its
        // query structures during `step`, and every caller of `sweep_box` in
        // the game is asking between ticks.
        env.step();
        assert!(
            env.sweep_box(Vec3::splat(16.0), Vec3::ZERO, Vec3::new(128.0, 0.0, 0.0))
                .is_some(),
            "an unheld cube stops a sweep"
        );

        let grab = GrabController::attach(&mut env, body);
        env.step();
        assert!(
            env.sweep_box(Vec3::splat(16.0), Vec3::ZERO, Vec3::new(128.0, 0.0, 0.0))
                .is_none(),
            "a held cube must not stop the sweep of the player holding it"
        );
        grab.detach(&mut env, Vec3::ZERO, 175.0);
        env.step();
        assert!(
            env.sweep_box(Vec3::splat(16.0), Vec3::ZERO, Vec3::new(128.0, 0.0, 0.0))
                .is_some(),
            "and must stop it again once it is put down"
        );
    }

    /// `angular_delta` is world-space and shortest-arc — the two things
    /// `physics_shadow.cpp`'s local copies get right and a transliteration
    /// gets wrong.
    #[test]
    fn the_angular_delta_is_world_space_and_takes_the_short_way_round() {
        // A 90° yaw from identity is +90° about world Z.
        let delta = angular_delta(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), Quat::IDENTITY);
        assert!((delta - Vec3::Z * std::f32::consts::FRAC_PI_2).length() < 1e-4, "{delta}");

        // 359° about Z is -1°, not +359°.
        let delta = angular_delta(
            Quat::from_rotation_z(359.0f32.to_radians()),
            Quat::IDENTITY,
        );
        assert!(delta.length() < 2.0f32.to_radians(), "{delta}");

        // And the frame is the *world's*: the same correction, asked of a body
        // that is already rolled 90° about X, is still about world Z.
        let current = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
        let target = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2) * current;
        let delta = angular_delta(target, current);
        assert!((delta - Vec3::Z * std::f32::consts::FRAC_PI_2).length() < 1e-4, "{delta}");
    }

    /// The scalar overload clamps by *magnitude*. The per-axis one in
    /// [`shadow`](super::super::shadow) would let a diagonal through at `√3`
    /// times the limit, which is the whole reason this function exists.
    #[test]
    fn the_scalar_controller_clamps_the_whole_vector_not_each_axis() {
        let mut velocity = Vec3::ZERO;
        compute_controller(&mut velocity, Vec3::splat(1000.0), 100.0, 100.0, 1.0, 1.0);
        assert!(
            (velocity.length() - 100.0).abs() < 1e-3,
            "expected a magnitude clamp to 100, got {} ({velocity})",
            velocity.length()
        );
    }
}
