//! Valve's angle conventions, which `glam` has no equivalent for.
//!
//! `PORTING.md` replaces `mathlib` with `glam`, and almost all of it goes:
//! dot products, cross products, matrix multiplication and projection are
//! `glam`'s and are better there. What does *not* go is the handful of places
//! where Valve fixed a convention — which axis a `QAngle`'s first component
//! turns about, and in which order the three compose. Those are not maths,
//! they are a data format: every `.bsp`, `.mdl` and `.vmf` in the game is
//! written against them, and `glam` has no opinion to inherit.
//!
//! This module is the crate root's second exception to "one module per Valve
//! module", for the same reason [`cmdline`](crate::cmdline) is the first:
//! its consumers are in `engine/` and in `client/`, which are siblings, so
//! there is nowhere below the root that can serve both.

use glam::{Mat3, Vec3};

/// `AngleMatrix` (`mathlib/mathlib_base.cpp:1305`) — a `QAngle`'s rotation.
///
/// **A `QAngle` is pitch, yaw, roll, in degrees**, and the composition is
/// `Rz(yaw) · Ry(pitch) · Rx(roll)` — the original's own comment reads
/// `matrix = (YAW * PITCH) * ROLL`. Built from three explicit axis rotations
/// rather than from `Mat3::from_euler`, because every `EulerRot` variant
/// encodes an intrinsic/extrinsic convention as well as an order, and picking
/// the wrong one is a silent half-right answer: anything with only a yaw looks
/// correct under any reading and everything tilted does not.
///
/// The result multiplies on the left, `angle_matrix(a) * v`, which is
/// `VectorRotate( v, matrix, out )` (`mathlib_base.cpp:323`) — Valve's
/// `matrix3x4_t` is row-major and `glam`'s is column-major, and the two
/// conventions cancel, so the entries are the same ones
/// `mathlib_base.cpp:1329` writes. The **inverse** is the transpose, which is
/// `VectorIRotate` (`:375`); there is no scale here to spoil that.
///
/// Translation is deliberately not included, where Valve's three-argument
/// overload includes it: the two consumers compose it differently — a static
/// prop wants a `Mat4` to draw with, a brush-model trace wants the rotation
/// alone so it can transpose it — and an unused column is a thing to get
/// wrong.
pub fn angle_matrix(angles: Vec3) -> Mat3 {
    let (pitch, yaw, roll) = (
        angles.x.to_radians(),
        angles.y.to_radians(),
        angles.z.to_radians(),
    );
    Mat3::from_rotation_z(yaw) * Mat3::from_rotation_y(pitch) * Mat3::from_rotation_x(roll)
}

/// `AngleVectors` (`mathlib/mathlib_base.cpp:1027`) — forward, right, up for
/// a `QAngle`.
///
/// Derived from [`angle_matrix`]'s columns rather than from its own trig, so
/// that the two cannot drift: column 0 is forward, column 1 is **left** — so
/// right is its negation — and column 2 is up. Source is Z-up right-handed and
/// "right" is `-Y` when facing `+X`, which is the sign that makes
/// `+moveright` add `right * cl_sidespeed`.
///
/// [`crate::client::view::ViewAngles::vectors`] is the same function for the
/// client's own angle type and spells the trig out; a test pins the two
/// together.
pub fn angle_vectors(angles: Vec3) -> (Vec3, Vec3, Vec3) {
    let matrix = angle_matrix(angles);
    (matrix.x_axis, -matrix.y_axis, matrix.z_axis)
}

/// `MatrixAngles` (`mathlib/mathlib_base.cpp:217`) — the `QAngle` a rotation
/// came from, which is [`angle_matrix`]'s inverse.
///
/// The columns are read the way Valve reads them: column 0 is forward, column
/// 1 is **left**, column 2 is up — see [`angle_matrix`] for why this port's
/// column-major `Mat3` and Valve's row-major `matrix3x4_t` index the same
/// entries.
///
/// **This is not [`vector_angles`] with the basis read out of a matrix**, and
/// the two deliberately disagree in the gimbal-locked case: `vector_angles`
/// negates the yaw there, carrying Valve's own note that the copy taken from
/// this function was 180° out. The general case agrees.
///
/// The caller is the teleport: an angle set goes through the portal matrix by
/// being turned into a rotation, composed, and read back out — Valve's
/// `UTIL_Portal_AngleTransform` (`portal_util_shared.cpp:1516`).
pub fn matrix_angles(matrix: Mat3) -> Vec3 {
    let forward = matrix.x_axis;
    let left = matrix.y_axis;
    // Only the z of up is needed, which is the comment in the original.
    let up_z = matrix.z_axis.z;

    let xy_dist = (forward.x * forward.x + forward.y * forward.y).sqrt();
    let pitch = (-forward.z).atan2(xy_dist).to_degrees();

    if xy_dist > 0.001 {
        Vec3::new(
            pitch,
            forward.y.atan2(forward.x).to_degrees(),
            left.z.atan2(up_z).to_degrees(),
        )
    } else {
        // Forward is (nearly) the z axis: yaw comes off the left vector and
        // roll is not recoverable, so it is assumed to be zero.
        Vec3::new(pitch, (-left.x).atan2(left.y).to_degrees(), 0.0)
    }
}

/// `VectorAngles( forward, pseudoup, angles )`
/// (`mathlib/mathlib_base.cpp:1142`) — the inverse of [`angle_matrix`] for a
/// direction plus a roll reference.
///
/// The `QAngle` that would make [`angle_matrix`]'s first column `forward`, with
/// the roll chosen so that the result's up is as close to `pseudo_up` as the
/// remaining freedom allows. Its caller in this port is the `portal` console
/// command, which is `CProp_Portal::ActivatePortal`'s
/// `VectorAngles( tr.plane.normal, vUp, qAngles )`: put a portal flat on
/// whatever surface was hit, rolled to stand up the way the player is standing.
///
/// Three things about it that are Valve's and not arithmetic:
///
/// - **`left` is `pseudo_up × forward`**, which is the same handedness
///   [`angle_matrix`]'s second column has. Taking the cross the other way round
///   rolls every result 180°.
/// - **Pitch is negated**, `atan2( -forward.z, xy )` — the comment in the
///   original says the engine's own sign is the opposite and that the game DLL
///   always negates it back.
/// - **Straight up or straight down loses roll entirely.** Below the `0.001`
///   guard yaw is read off `left` instead, and roll is *assumed zero* because
///   one degree of freedom has gone. The yaw branch there carries Valve's own
///   note that it was copied from `MatrixAngles`, found to be 180° out, and
///   negated — so the two functions disagree on purpose in exactly that case.
///
/// `pseudo_up` need not be perpendicular to `forward`; it only has to be
/// non-parallel, which is what "pseudo" means here.
pub fn vector_angles(forward: Vec3, pseudo_up: Vec3) -> Vec3 {
    let left = pseudo_up.cross(forward).normalize_or_zero();
    let xy_dist = (forward.x * forward.x + forward.y * forward.y).sqrt();
    let pitch = (-forward.z).atan2(xy_dist).to_degrees();

    if xy_dist > 0.001 {
        let yaw = forward.y.atan2(forward.x).to_degrees();
        let up_z = left.y * forward.x - left.x * forward.y;
        let roll = left.z.atan2(up_z).to_degrees();
        Vec3::new(pitch, yaw, roll)
    } else {
        // Gimbal lock: forward is (nearly) the z axis, so yaw comes off the
        // left vector and roll is not recoverable.
        Vec3::new(pitch, (-left.x).atan2(left.y).to_degrees(), 0.0)
    }
}

#[cfg(test)]
mod tests {

    /// [`angle_vectors`] must agree with the client's own spelled-out
    /// `AngleVectors`, or the carry direction and the view would disagree.
    #[test]
    fn angle_vectors_agrees_with_the_client_view() {
        for angles in [
            Vec3::ZERO,
            Vec3::new(0.0, 90.0, 0.0),
            Vec3::new(-30.0, 45.0, 0.0),
            Vec3::new(75.0, -170.0, 0.0),
            Vec3::new(12.5, 200.0, 33.0),
        ] {
            let view = crate::client::view::ViewAngles {
                pitch: angles.x,
                yaw: angles.y,
                roll: angles.z,
            };
            let (ef, er, eu) = view.vectors();
            let (f, r, u) = angle_vectors(angles);
            assert!((f - ef).length() < 1e-5, "forward {angles}: {f} vs {ef}");
            assert!((r - er).length() < 1e-5, "right {angles}: {r} vs {er}");
            assert!((u - eu).length() < 1e-5, "up {angles}: {u} vs {eu}");
        }
    }
    use super::*;

    /// Each axis against the column of `matrix3x4_t` the original writes,
    /// which is the only way to catch a sign: pitch in particular rotates
    /// `+X` towards `-Z`, not `+Z`.
    ///
    /// The same assertions guard
    /// [`StaticProp::rotation`](crate::engine::world::props::StaticProp::rotation),
    /// which is this function's other caller; they are here because this is
    /// where the convention now lives.
    #[test]
    fn valves_angle_order_is_yaw_then_pitch_then_roll() {
        let close = |a: Vec3, b: Vec3| assert!((a - b).length() < 1e-5, "{a} vs {b}");

        // Yaw alone: +X to +Y.
        close(angle_matrix(Vec3::new(0.0, 90.0, 0.0)) * Vec3::X, Vec3::Y);
        // Pitch alone: +X to -Z. `matrix[2][0] = -sp`.
        close(angle_matrix(Vec3::new(90.0, 0.0, 0.0)) * Vec3::X, -Vec3::Z);
        // Roll alone: +Y to +Z. `matrix[2][1] = sr*cp`.
        close(angle_matrix(Vec3::new(0.0, 0.0, 90.0)) * Vec3::Y, Vec3::Z);

        // All three together, against `AngleMatrix` evaluated by hand at
        // pitch 30, yaw 45, roll 60 — the case that distinguishes this order
        // from every other one.
        let (p, y, r) = (30f32.to_radians(), 45f32.to_radians(), 60f32.to_radians());
        let (sp, cp) = p.sin_cos();
        let (sy, cy) = y.sin_cos();
        let (sr, cr) = r.sin_cos();
        let m = angle_matrix(Vec3::new(30.0, 45.0, 60.0));
        close(m * Vec3::X, Vec3::new(cp * cy, cp * sy, -sp));
        close(
            m * Vec3::Y,
            Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp),
        );
        close(
            m * Vec3::Z,
            Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp),
        );
    }

    /// [`vector_angles`] is [`angle_matrix`]'s inverse for the pair it is
    /// given, for every orientation that has a roll to recover.
    ///
    /// The round trip is the test that catches a sign in either function: an
    /// angle triple goes to a matrix, its forward and up come back out, and
    /// the angles rebuilt from those two must rotate the same way. Pitches at
    /// ±90 are excluded because roll genuinely is not recoverable there —
    /// which is the `xyDist > 0.001` branch, and is checked separately below.
    #[test]
    fn vector_angles_inverts_angle_matrix() {
        for &(pitch, yaw, roll) in &[
            (0.0, 0.0, 0.0),
            (0.0, 90.0, 0.0),
            (0.0, 180.0, 0.0),
            (-0.5, 0.0, 0.0),
            (30.0, 45.0, 60.0),
            (-75.4673, 21.1748, 6.802),
            (-5.5, 0.0, 0.0),
            (89.0, 12.0, -170.0),
        ] {
            let m = angle_matrix(Vec3::new(pitch, yaw, roll));
            let (forward, up) = (m * Vec3::X, m * Vec3::Z);
            let back = angle_matrix(vector_angles(forward, up));
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                let (a, b) = (m * axis, back * axis);
                assert!((a - b).length() < 1e-4, "{pitch} {yaw} {roll}: {a} vs {b}");
            }
        }
    }

    /// The gimbal-lock branch: a portal on the floor or the ceiling. The
    /// forward axis still has to come back exactly; the roll is discarded and
    /// the yaw absorbs it, which is the degree of freedom that was lost.
    #[test]
    fn a_straight_up_direction_keeps_its_forward_and_loses_its_roll() {
        for (forward, up) in [(Vec3::Z, Vec3::X), (-Vec3::Z, Vec3::X), (Vec3::Z, Vec3::Y)] {
            let angles = vector_angles(forward, up);
            assert_eq!(angles.z, 0.0, "roll is not recoverable here");
            let back = angle_matrix(angles) * Vec3::X;
            assert!((back - forward).length() < 1e-5, "{back} vs {forward}");
        }
    }

    /// The transpose is the inverse — which is what `VectorIRotate` assumes
    /// and what a brush-model trace relies on to get back out of local space.
    #[test]
    fn the_transpose_undoes_the_rotation() {
        let m = angle_matrix(Vec3::new(30.0, 45.0, 60.0));
        let v = Vec3::new(3.0, -7.0, 11.0);
        assert!((m.transpose() * (m * v) - v).length() < 1e-4);
        assert!((m * (m.transpose() * v) - v).length() < 1e-4);
    }

    /// [`matrix_angles`] undoes [`angle_matrix`] — which is what makes the
    /// teleport's angle transform a *compose* rather than a special case per
    /// axis.
    ///
    /// Sets that exercise all three components, including a pitch past
    /// vertical and a roll, because the roll branch reads a different pair of
    /// matrix entries from the yaw one.
    #[test]
    fn matrix_angles_reads_back_what_angle_matrix_wrote() {
        let close = |a: Vec3, b: Vec3| {
            let wrapped = |v: f32| (v + 180.0).rem_euclid(360.0) - 180.0;
            let diff = Vec3::new(wrapped(a.x - b.x), wrapped(a.y - b.y), wrapped(a.z - b.z));
            assert!(diff.length() < 1e-3, "{a} vs {b}");
        };

        for angles in [
            Vec3::ZERO,
            Vec3::new(0.0, 90.0, 0.0),
            Vec3::new(0.0, 180.0, 0.0),
            Vec3::new(-30.0, 45.0, 0.0),
            Vec3::new(20.0, -120.0, 15.0),
            Vec3::new(-75.0, 10.0, -60.0),
        ] {
            close(matrix_angles(angle_matrix(angles)), angles);
        }
    }

    /// The gimbal-locked case, where one degree of freedom is gone: the roll is
    /// *assumed zero* rather than recovered, and the yaw absorbs it.
    ///
    /// **This is where [`matrix_angles`] and [`vector_angles`] disagree on
    /// purpose** — the original carries a note that the copy taken from
    /// `MatrixAngles` was found to be 180° out — so neither is a drop-in for
    /// the other.
    #[test]
    fn straight_up_loses_the_roll_and_keeps_the_direction() {
        let straight_up = Vec3::new(-90.0, 30.0, 0.0);
        let read_back = matrix_angles(angle_matrix(straight_up));
        assert_eq!(read_back.z, 0.0);
        assert!((read_back.x + 90.0).abs() < 1e-3, "{read_back}");
        // The direction survives even though the angles do not have to.
        let forward = |a: Vec3| angle_matrix(a) * Vec3::X;
        assert!((forward(read_back) - forward(straight_up)).length() < 1e-4);
    }
}
