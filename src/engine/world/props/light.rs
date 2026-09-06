//! Lighting a static prop from the map's baked ambient cubes.
//!
//! `Mod_LeafAmbientColorAtPos` (`engine/modelloader.cpp:7301`) and the part of
//! the studio lighting setup that feeds it into a material. Stage 5 of
//! `portdocs/STUDIO.md` §8.
//!
//! `vrad` bakes several light cubes per leaf, positioned as fixed-point
//! fractions of the leaf's bounding box, and the runtime reconstructs the
//! lighting at a point by inverse-square-distance-weighting them. That is what
//! makes two props in one room light differently, and it is the whole of a
//! static prop's lighting until the `.vhv` per-vertex bake lands (stage 4).
//!
//! # Not ported
//!
//! The local lights. `LightcacheGetStatic` also walks `dworldlight_t[]` for the
//! four brightest lights reaching the point and fills
//! [`ModelLighting::lights`]; that needs `LUMP_WORLDLIGHTS`, the light-type
//! attenuation model and the visibility check that decides whether a light
//! reaches a point, none of which exist here yet. Props are lit by the ambient
//! cube alone, which is dimmer and flatter than the shipped game and is not
//! wrong in the way an unlit prop is.
//!
//! [`ModelLighting::lights`]: crate::materials::uniforms::ModelLighting::lights

use glam::Vec3;

use crate::engine::trace::CollisionBsp;
use crate::engine::world::bsp::{Bsp, LeafAmbientIndex, LeafAmbientSample};
use crate::materials::uniforms::{Light, ModelLighting, AMBIENT_CUBE_FACES};

/// The six ambient-cube faces, in `+x, -x, +y, -y, +z, -z` order.
pub type AmbientCube = [[f32; 3]; AMBIENT_CUBE_FACES];

/// Reconstructs the baked ambient lighting at a world position.
///
/// `Mod_LeafAmbientColorAtPos`. Returns black when the map has no baked ambient
/// lighting at all, which is a map compiled without `vrad`.
///
/// The solid-leaf redirect is [`samples_for`]'s; the interpolation is
/// [`reconstruct`]'s.
pub fn ambient_at(bsp: &Bsp, collision: &CollisionBsp, position: Vec3) -> AmbientCube {
    let Some((leaf_index, first, count)) =
        samples_for(&bsp.leaf_ambient_index, collision.leaf(position))
    else {
        return [[0.0; 3]; AMBIENT_CUBE_FACES];
    };
    let (Some(leaf), Some(samples)) = (bsp.leaves.get(leaf_index), bsp.leaf_ambient.get(first..first + count))
    else {
        return [[0.0; 3]; AMBIENT_CUBE_FACES];
    };
    let bounds = |v: [i16; 3]| Vec3::new(f32::from(v[0]), f32::from(v[1]), f32::from(v[2]));
    reconstruct(bounds(leaf.mins), bounds(leaf.maxs), samples, position)
}

/// Which leaf's samples light a point in `leaf`, and which range of them.
///
/// **The solid-leaf redirect lives here.** A leaf with zero samples and a
/// non-zero `first_sample` is a *solid* leaf, and its `first_sample` is a
/// **leaf index**, not a sample index — `vrad` writes it so that a prop
/// embedded in geometry borrows a neighbour's lighting instead of going black.
/// The field means two different things depending on the count beside it, and
/// nothing in the lump says so (`modelloader.cpp:7309`).
fn samples_for(index: &[LeafAmbientIndex], leaf: usize) -> Option<(usize, usize, usize)> {
    let mut leaf = leaf;
    let mut ambient = *index.get(leaf)?;
    if ambient.sample_count == 0 && ambient.first_sample != 0 {
        leaf = ambient.first_sample as usize;
        ambient = *index.get(leaf)?;
    }
    if ambient.sample_count == 0 {
        return None;
    }
    Some((
        leaf,
        ambient.first_sample as usize,
        ambient.sample_count as usize,
    ))
}

/// The interpolation itself, given a leaf's bounds and its samples.
///
/// `Mod_LeafAmbientColorAtPos`' inner loop, split out from the lookup so it can
/// be tested without a BSP tree.
fn reconstruct(
    mins: Vec3,
    maxs: Vec3,
    samples: &[LeafAmbientSample],
    position: Vec3,
) -> AmbientCube {
    let mut out = [[0.0f32; 3]; AMBIENT_CUBE_FACES];
    let mut total = 0.0f32;
    for sample in samples {
        // The original works from the leaf's centre and half-diagonal and
        // scales by `2/255`; that is `mins + (xyz/255) * (maxs - mins)` with
        // the halves cancelled, and this form does not need the centre.
        let fraction = Vec3::new(
            f32::from(sample.x) / 255.0,
            f32::from(sample.y) / 255.0,
            f32::from(sample.z) / 255.0,
        );
        let sample_position = mins + fraction * (maxs - mins);

        // Inverse *squared* distance, with the `+1` that keeps a sample the
        // prop is standing on from dominating to infinity.
        let factor = 1.0 / (sample_position.distance_squared(position) + 1.0);
        total += factor;
        for (face, colour) in sample.cube.iter().enumerate() {
            let linear = decode(*colour);
            for channel in 0..3 {
                out[face][channel] += linear[channel] * factor;
            }
        }
    }

    if total > 0.0 {
        for face in &mut out {
            for channel in face.iter_mut() {
                *channel /= total;
            }
        }
    }
    out
}

/// One `ColorRGBExp32` of an ambient cube, in the units this port's shaders
/// light with.
///
/// **`ColorRGBExp32ToVector`, not `TexLightToLinear`** — the opposite of the
/// rule the lightmap path follows, and the two differ by exactly 255. The
/// original decodes an ambient cube this way (`modelloader.cpp:7338`) and a
/// lightmap luxel the other (`gl_lightmap.cpp:572`), which reads like an
/// inconsistency and is not: the two values reach the GPU by different routes.
///
/// It was checked rather than assumed, because this port has one linear space
/// that both `LightmappedGeneric` and `VertexLitGeneric` sample directly and
/// getting it backwards is a factor of 255 either way. Over `sp_a1_intro1`,
/// mean luminance under `TexLightToLinear` is 0.0249 for the lightmap and
/// 0.0002 for the ambient cubes — 122× apart, which this closes to 0.5×. Props
/// decoded the lightmap's way are black.
fn decode(colour: crate::materials::lightmap::ColorRgbExp32) -> [f32; 3] {
    colour.to_vector()
}

/// The lighting one prop is drawn with.
///
/// The ambient cube from [`ambient_at`], no local lights, and no baked static
/// light — the `.vhv` stream is stage 4, and until it lands
/// [`ModelLighting::static_light`] stays 0 so the shader does not read a black
/// colour stream as real darkness.
pub fn lighting_for(bsp: &Bsp, collision: &CollisionBsp, position: Vec3) -> ModelLighting {
    let cube = ambient_at(bsp, collision, position);
    let mut ambient_cube = [[0.0f32; 4]; AMBIENT_CUBE_FACES];
    for (out, face) in ambient_cube.iter_mut().zip(cube) {
        *out = [face[0], face[1], face[2], 0.0];
    }
    ModelLighting {
        ambient_cube,
        lights: [Light::NONE; 4],
        count: 0,
        static_light: 0,
        ambient_light: 1,
        _padding: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::lightmap::ColorRgbExp32;

    /// A sample at `(x, y, z)` of the leaf's bounds whose whole cube is one
    /// grey value.
    fn sample(x: u8, y: u8, z: u8, value: u8) -> LeafAmbientSample {
        LeafAmbientSample {
            cube: [ColorRgbExp32 {
                r: value,
                g: value,
                b: value,
                // 2^0 = 1, so `to_vector` gives the mantissa back unchanged.
                exponent: 0,
            }; 6],
            x,
            y,
            z,
            _pad: 0,
        }
    }

    const MINS: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    const MAXS: Vec3 = Vec3::new(255.0, 255.0, 255.0);

    /// The sample positions are fractions of the leaf's bounds, not world
    /// coordinates. Getting that wrong puts every sample at the map's origin
    /// and lights the whole level from one point.
    #[test]
    fn a_sample_sits_where_its_fraction_of_the_leaf_says() {
        // One sample, so the answer is that sample wherever it is asked from.
        let one = [sample(255, 255, 255, 100)];
        let at_it = reconstruct(MINS, MAXS, &one, Vec3::splat(255.0));
        let far = reconstruct(MINS, MAXS, &one, Vec3::ZERO);
        assert_eq!(at_it, far, "one sample is the whole answer");
        assert!((at_it[0][0] - 100.0).abs() < 1e-3);
    }

    /// Inverse *squared* distance: the nearer sample dominates, and it is not
    /// a plain average.
    #[test]
    fn the_nearer_sample_dominates() {
        let two = [sample(0, 0, 0, 200), sample(255, 255, 255, 0)];
        let near_dark = reconstruct(MINS, MAXS, &two, Vec3::splat(250.0));
        let near_bright = reconstruct(MINS, MAXS, &two, Vec3::splat(5.0));
        assert!(near_bright[0][0] > 150.0, "{:?}", near_bright[0]);
        assert!(near_dark[0][0] < 50.0, "{:?}", near_dark[0]);
        // Exactly between them it is the average, which is where a plain mean
        // and this agree and so is not what distinguishes them.
        let middle = reconstruct(MINS, MAXS, &two, Vec3::splat(127.5));
        assert!((middle[0][0] - 100.0).abs() < 1.0, "{:?}", middle[0]);
    }

    /// A leaf with samples uses its own; a solid leaf with none borrows the
    /// leaf its `first_sample` names.
    #[test]
    fn a_solid_leaf_borrows_its_neighbours_samples() {
        let index = [
            // Leaf 0: solid, redirects to leaf 1.
            LeafAmbientIndex {
                sample_count: 0,
                first_sample: 1,
            },
            // Leaf 1: two real samples starting at 5.
            LeafAmbientIndex {
                sample_count: 2,
                first_sample: 5,
            },
            // Leaf 2: genuinely empty — no samples, no redirect.
            LeafAmbientIndex {
                sample_count: 0,
                first_sample: 0,
            },
        ];
        assert_eq!(samples_for(&index, 0), Some((1, 5, 2)), "the redirect");
        assert_eq!(samples_for(&index, 1), Some((1, 5, 2)));
        assert_eq!(samples_for(&index, 2), None);
        assert_eq!(samples_for(&index, 9), None, "past the end");
    }

    /// The ambient cube decodes with `ColorRGBExp32ToVector` and not with the
    /// lightmap's `TexLightToLinear`. See [`decode`] — the difference is 255×
    /// and it is the difference between lit props and black ones.
    #[test]
    fn the_cube_decodes_the_ambient_way_and_not_the_lightmap_way() {
        let one = [sample(0, 0, 0, 51)];
        let cube = reconstruct(MINS, MAXS, &one, Vec3::ZERO);
        assert!((cube[0][0] - 51.0).abs() < 1e-3, "{:?}", cube[0]);
        // What the lightmap decode would have given.
        assert!((cube[0][0] - 51.0 / 255.0).abs() > 1.0);
    }

    /// A map with no baked ambient lighting gives black rather than a panic,
    /// and `lighting_for` still produces a valid state.
    #[test]
    fn no_samples_is_black_and_not_an_error() {
        assert_eq!(reconstruct(MINS, MAXS, &[], Vec3::ZERO), [[0.0; 3]; 6]);
    }
}
