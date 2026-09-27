//! `portal_placement.cpp`, exercised through the real brush tracer.
//!
//! Every world here is an [`engine::trace::fixture`](crate::engine::trace::fixture)
//! — real `CollisionBsp` brushes, traced by the same code the game uses —
//! so the fit is tested against real `fractionleftsolid`s and real
//! `DIST_EPSILON` gaps rather than a mock that agrees with it by
//! construction.
//!
//! The standard room is a wall whose face is the plane `x = 0`, facing `-x`,
//! 256 wide (`y` in ±128) and 256 high (`z` in 0..256), with a floor under it
//! at `z = 0`. The shooter stands at `x = -256` and fires along `+x`.

use glam::Vec3;

use super::*;
use crate::engine::trace::fixture::Fixture;
use crate::engine::trace::{BrushModel, CollisionBsp, Contents};
use crate::server::portalgun::trace_fire_portal;

/// `SURF_SKY`, `SURF_NOPORTAL`.
const SKY: i32 = 0x0004;
const NOPORTAL: i32 = 0x0020;

const HALF_WIDTH: f32 = 32.0;
const HALF_HEIGHT: f32 = 56.0;

/// A collision model the gun can be fired into — the world, and brush model
/// 1 for a volume to live in.
pub(crate) struct FixtureWorld {
    collision: CollisionBsp,
    chain: Vec<BrushModel>,
}

impl TouchQuery for FixtureWorld {
    fn brush_models_touching(&mut self, _: Vec3, _: Vec3, _: Vec3, _: Vec3, _: &mut Vec<usize>) {}

    fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
        false
    }

    fn shot_trace(&mut self, start: Vec3, end: Vec3, mask: u32) -> ShotHit {
        crate::engine::shot_trace(&self.collision, &[], &mut self.chain, start, end, mask)
    }

    fn clip_to_model(
        &mut self,
        model: usize,
        origin: Vec3,
        angles: Vec3,
        start: Vec3,
        end: Vec3,
        mask: u32,
    ) -> ShotHit {
        crate::engine::clip_to_model(&self.collision, model, origin, angles, start, end, mask)
    }

    fn surface_name(&self, surface: u16) -> String {
        self.collision.surface_name(Some(surface)).to_owned()
    }
}

/// A world built by `build`, with any material whose name contains "glass"
/// resolved to `CHAR_TEX_GLASS` and everything else to concrete. The brushes
/// `build` returns are brush model 1's rather than the world's.
pub(crate) fn world_with_model(build: impl FnOnce(&mut Fixture) -> Vec<u16>) -> FixtureWorld {
    let mut fixture = Fixture::default();
    let model = build(&mut fixture);
    let world: Vec<u16> = (0..fixture.brushes.len() as u16)
        .filter(|b| !model.contains(b))
        .collect();
    let mut collision = fixture.world_and_model(&world, &model);
    collision.resolve_game_materials(|name| match name.contains("glass") {
        true => u16::from(b'Y'),
        false => u16::from(b'C'),
    });
    FixtureWorld {
        collision,
        chain: Vec::new(),
    }
}

pub(crate) fn world(build: impl FnOnce(&mut Fixture)) -> FixtureWorld {
    world_with_model(|f| {
        build(f);
        Vec::new()
    })
}

/// The standard room: a concrete wall at `x = 0` and a floor below it.
pub(crate) fn wall(fixture: &mut Fixture) {
    fixture.add_named_box(
        Vec3::new(0.0, -128.0, 0.0),
        Vec3::new(16.0, 128.0, 256.0),
        Contents::SOLID,
        "concrete/wall",
        0,
    );
    fixture.add_named_box(
        Vec3::new(-512.0, -512.0, -16.0),
        Vec3::new(0.0, 512.0, 0.0),
        Contents::SOLID,
        "concrete/floor",
        0,
    );
}

/// Fire from `x = -256` at the point `(y, z)` on the wall.
fn shoot_at(world: &mut FixtureWorld, y: f32, z: f32) -> crate::server::portalgun::Shot {
    let mut shot = ShotWorld::bare(world);
    shoot_with(&mut shot, y, z)
}

fn shoot_with(shot: &mut ShotWorld<'_>, y: f32, z: f32) -> crate::server::portalgun::Shot {
    trace_fire_portal(
        shot,
        EntityId::INVALID,
        Vec3::new(-256.0, y, z),
        Vec3::X,
        HALF_WIDTH,
        HALF_HEIGHT,
        PlacedBy::Player,
    )
}

#[test]
fn a_shot_into_the_middle_of_a_wall_places_where_it_hit() {
    let mut w = world(wall);
    let shot = shoot_at(&mut w, 0.0, 128.0);
    // `VerifyPortalPlacement` returns `BUMPED` for every success, including
    // one that did not move — Valve's, and why no caller sees `SUCCESS`.
    assert_eq!(shot.result, PlacementResult::Bumped);
    assert!((shot.position - Vec3::new(-0.03125, 0.0, 128.0)).abs().max_element() < 0.01, "{shot:?}");
    // Facing out of the wall: yaw 180, no pitch, no roll.
    let (forward, _, up) = angle_vectors(shot.angles);
    assert!((forward - Vec3::NEG_X).length() < 1e-4, "{forward}");
    assert!((up - Vec3::Z).length() < 1e-4, "{up}");
}

/// A shot 18 units from the wall's edge: the far corner hangs 12 units off
/// the surface, and the fit slides the portal in until its **real** edge —
/// not its forgiveness-shrunk corner — is flush with the wall's.
#[test]
fn a_shot_near_the_edge_of_a_wall_is_bumped_back_onto_it() {
    let mut w = world(wall);
    let shot = shoot_at(&mut w, 110.0, 128.0);
    assert_eq!(shot.result, PlacementResult::Bumped);
    assert!(
        (shot.position.y - (128.0 - HALF_WIDTH)).abs() < 0.1,
        "the edge is flush with the wall's: {shot:?}"
    );
    assert!((shot.position.z - 128.0).abs() < 0.01, "and nothing moved vertically");
}

/// The bottom corners are in the floor: the enclosing-wall trace (one unit
/// in front of the wall) finds it and the portal goes up — and then, being
/// within a unit and a half of the floor, is snapped down onto it exactly,
/// *"to help game movement code run smoothly"*.
#[test]
fn a_shot_near_the_floor_is_bumped_up_and_snapped_to_it() {
    let mut w = world(wall);
    let shot = shoot_at(&mut w, 0.0, 20.0);
    assert_eq!(shot.result, PlacementResult::Bumped);
    // The floor trace stops `DIST_EPSILON` short of `z = 0`.
    assert!(
        (shot.position.z - (HALF_HEIGHT + 0.03125)).abs() < 0.01,
        "bottom edge on the floor: {shot:?}"
    );
}

#[test]
fn a_noportal_surface_refuses_the_portal() {
    let mut w = world(|f| {
        f.add_named_box(
            Vec3::new(0.0, -128.0, 0.0),
            Vec3::new(16.0, 128.0, 256.0),
            Contents::SOLID,
            "metal/black_wall",
            NOPORTAL,
        );
    });
    let shot = shoot_at(&mut w, 0.0, 128.0);
    assert_eq!(shot.result, PlacementResult::InvalidSurface);
}

/// 4,302 shipped brush sides are glass with no `SURF_NOPORTAL` on them —
/// mostly ceiling light panels. `CHAR_TEX_GLASS` alone keeps a portal off.
#[test]
fn glass_refuses_the_portal_without_any_flag() {
    let mut w = world(|f| {
        f.add_named_box(
            Vec3::new(0.0, -128.0, 0.0),
            Vec3::new(16.0, 128.0, 256.0),
            Contents::SOLID,
            "lights/light_panel_glass",
            0,
        );
    });
    let shot = shoot_at(&mut w, 0.0, 128.0);
    assert_eq!(shot.result, PlacementResult::InvalidSurface);
}

#[test]
fn sky_is_a_surface_the_shot_passes_through() {
    let mut w = world(|f| {
        f.add_named_box(
            Vec3::new(0.0, -128.0, 0.0),
            Vec3::new(16.0, 128.0, 256.0),
            Contents::SOLID,
            "tools/toolsskybox",
            SKY,
        );
    });
    let shot = shoot_at(&mut w, 0.0, 128.0);
    assert_eq!(shot.result, PlacementResult::PassthroughSurface);
}

#[test]
fn a_shot_at_nothing_is_a_pass_through() {
    let mut w = world(|_| {});
    let shot = shoot_at(&mut w, 0.0, 128.0);
    assert_eq!(shot.result, PlacementResult::PassthroughSurface);
}

/// A portalable wall with a no-portal strip beside it, flush in the same
/// plane. The corner over the strip has no wall to hit and no edge to find —
/// the strip is as solid as the wall — so it is the corner's own material
/// check and its binary search that find the boundary and slide the portal
/// off it.
#[test]
fn a_portal_overlapping_a_noportal_strip_is_slid_off_it() {
    let mut w = world(|f| {
        f.add_named_box(
            Vec3::new(0.0, -128.0, 0.0),
            Vec3::new(16.0, 64.0, 256.0),
            Contents::SOLID,
            "concrete/wall",
            0,
        );
        f.add_named_box(
            Vec3::new(0.0, 64.0, 0.0),
            Vec3::new(16.0, 128.0, 256.0),
            Contents::SOLID,
            "metal/black_wall",
            NOPORTAL,
        );
    });
    let shot = shoot_at(&mut w, 50.0, 128.0);
    assert!(shot.result.succeeded(), "{shot:?}");
    assert!(
        shot.position.y + HALF_WIDTH <= 64.0 + PORTAL_BUMP_FORGIVENESS + 0.1,
        "the portal is off the strip, to within the forgiveness: {shot:?}"
    );
}

#[test]
fn a_wall_narrower_than_a_portal_cannot_fit_one() {
    let mut w = world(|f| {
        f.add_named_box(
            Vec3::new(0.0, -20.0, 0.0),
            Vec3::new(16.0, 20.0, 256.0),
            Contents::SOLID,
            "concrete/wall",
            0,
        );
    });
    let shot = shoot_at(&mut w, 0.0, 128.0);
    assert_eq!(shot.result, PlacementResult::CantFit);
}

/// Another portal already where the shot lands: `FitPortalAroundOtherPortals`
/// slides the new one sideways off it before the fit, by a portal's width and
/// a unit.
#[test]
fn a_shot_onto_another_portal_is_slid_beside_it() {
    let mut w = world(wall);
    let mut shot_world = ShotWorld::bare(&mut w);
    let other = EntityId::test(7);
    shot_world.portals.push(PortalInfo {
        id: other,
        origin: Vec3::new(-0.03125, 0.0, 128.0),
        angles: Vec3::new(0.0, 180.0, 0.0),
        half_width: HALF_WIDTH,
        half_height: HALF_HEIGHT,
        active: true,
        fired_by: None,
    });
    let shot = shoot_with(&mut shot_world, 4.0, 128.0);
    assert!(shot.result.succeeded(), "{shot:?}");
    assert!(
        (shot.position.y.abs() - (2.0 * HALF_WIDTH)).abs() < 1.5,
        "a width away, either side: {shot:?}"
    );
}

/// A `func_noportal_volume` is fitted away from like a wall two units nearer
/// than it is — so a shot beside one slides clear of it — and a shot into the
/// middle of one is refused.
///
/// The refusal comes from the **last** check, not the fit: the corner lines
/// all start inside the volume, the fit moves the portal as far as its
/// recursion allows without getting clear, and
/// `IsPortalIntersectingNoPortalVolume` then says no.
#[test]
fn a_no_portal_volume_is_fitted_away_from_and_refuses_a_shot_inside_it() {
    let (mins, maxs) = (Vec3::new(-8.0, 40.0, 0.0), Vec3::new(8.0, 128.0, 256.0));
    let mut w = world_with_model(|f| {
        wall(f);
        vec![f.add_box(mins, maxs, Contents::SOLID, true)]
    });
    let volume = Bumper {
        id: EntityId::test(3),
        kind: BumperKind::NoPortalVolume,
        model: 1,
        origin: Vec3::ZERO,
        angles: Vec3::ZERO,
        mins,
        maxs,
    };

    let mut shot_world = ShotWorld::bare(&mut w);
    shot_world.bumpers.push(volume);
    let beside = shoot_with(&mut shot_world, 20.0, 128.0);
    assert!(beside.result.succeeded(), "{beside:?}");
    assert!(
        beside.position.y + HALF_WIDTH <= 40.0,
        "clear of the volume: {beside:?}"
    );

    let inside = shoot_with(&mut shot_world, 90.0, 128.0);
    assert_eq!(inside.result, PlacementResult::InvalidVolume);
}

/// An enabled fizzler between the gun and the wall stops the shot at its own
/// box, whatever is behind it.
#[test]
fn a_fizzler_in_the_way_stops_the_shot() {
    let mut w = world(wall);
    let mut shot_world = ShotWorld::bare(&mut w);
    shot_world.bumpers.push(Bumper {
        id: EntityId::test(4),
        kind: BumperKind::Cleanser,
        model: 1,
        origin: Vec3::ZERO,
        angles: Vec3::ZERO,
        mins: Vec3::new(-100.0, -128.0, 0.0),
        maxs: Vec3::new(-96.0, 128.0, 256.0),
    });
    let shot = shoot_with(&mut shot_world, 0.0, 128.0);
    assert_eq!(shot.result, PlacementResult::Cleanser);
    assert!((shot.position.x - -100.0).abs() < 0.01, "at the field's near face: {shot:?}");
}

/// A shot landing inside a helper's radius goes to the helper's own spot on
/// the same surface, at the helper's angles when it says so.
#[test]
fn a_shot_near_a_placement_helper_snaps_to_it() {
    let mut w = world(wall);
    let mut shot_world = ShotWorld::bare(&mut w);
    shot_world.helpers.push(Helper {
        id: EntityId::test(5),
        origin: Vec3::new(0.0, -40.0, 100.0),
        angles: Vec3::new(0.0, 180.0, 0.0),
        radius: 48.0,
        use_angles: true,
    });
    let shot = shoot_with(&mut shot_world, -10.0, 120.0);
    assert_eq!(shot.result, PlacementResult::UsedHelper);
    assert!(
        (shot.position - Vec3::new(-0.03125, -40.0, 100.0)).abs().max_element() < 0.01,
        "{shot:?}"
    );

    // Out of its radius, the shot places where it landed.
    let far = shoot_with(&mut shot_world, 80.0, 180.0);
    assert_eq!(far.result, PlacementResult::Bumped);
}

#[test]
fn separating_axes_find_overlapping_and_apart_boxes() {
    use crate::server::obb::obb_intersects_obb;
    let unit = (Vec3::splat(-1.0), Vec3::splat(1.0));
    assert!(obb_intersects_obb(Vec3::ZERO, Vec3::ZERO, unit.0, unit.1, Vec3::new(1.5, 0.0, 0.0), Vec3::ZERO, unit.0, unit.1));
    assert!(!obb_intersects_obb(Vec3::ZERO, Vec3::ZERO, unit.0, unit.1, Vec3::new(2.5, 0.0, 0.0), Vec3::ZERO, unit.0, unit.1));
    // Turned 45° about z, a unit cube reaches √2 along x: 2.3 apart overlaps.
    let turned = Vec3::new(0.0, 45.0, 0.0);
    assert!(obb_intersects_obb(Vec3::ZERO, turned, unit.0, unit.1, Vec3::new(2.3, 0.0, 0.0), Vec3::ZERO, unit.0, unit.1));
    assert!(!obb_intersects_obb(Vec3::ZERO, turned, unit.0, unit.1, Vec3::new(2.5, 0.0, 0.0), Vec3::ZERO, unit.0, unit.1));
    // Touching counts: separation is strict.
    assert!(obb_intersects_obb(Vec3::ZERO, Vec3::ZERO, unit.0, unit.1, Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO, unit.0, unit.1));
}
