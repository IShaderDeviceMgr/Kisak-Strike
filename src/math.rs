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

#[cfg(test)]
mod tests {
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

    /// The transpose is the inverse — which is what `VectorIRotate` assumes
    /// and what a brush-model trace relies on to get back out of local space.
    #[test]
    fn the_transpose_undoes_the_rotation() {
        let m = angle_matrix(Vec3::new(30.0, 45.0, 60.0));
        let v = Vec3::new(3.0, -7.0, 11.0);
        assert!((m.transpose() * (m * v) - v).length() < 1e-4);
        assert!((m * (m.transpose() * v) - v).length() < 1e-4);
    }
}
