use std::collections::HashMap;

use glam::{Quat, Vec3};

use super::*;
use crate::engine::world::bsp;
use crate::server::classes::portal::teleport_matrix;
use crate::server::classes::MathCounter;
use crate::server::entity::EntityId;
use crate::server::PushHit;
use crate::server::{name, NoTouchQuery, PlayerState, Server};
use crate::vphysics::collide::{Ledge, Solid as CollideSolid, SolidParams};
use crate::vphysics::env::{Environment, Hulls, Mass, Motion};
use crate::vphysics::Model;

// ---------------------------------------------------------------------------
// The rules, on their own
// ---------------------------------------------------------------------------

/// A linked portal at `origin` facing along `angles`, whose partner is
/// `partner` — ids are made up; only [`LinkedPortal::id`] comparisons use them.
fn portal(slot: u32, origin: Vec3, angles: Vec3, partner: (u32, Vec3, Vec3)) -> LinkedPortal {
    let frame = angle_matrix(angles);
    LinkedPortal {
        id: EntityId::test(slot),
        linked: EntityId::test(partner.0),
        origin,
        forward: frame * Vec3::X,
        right: -(frame * Vec3::Y),
        up: frame * Vec3::Z,
        rotation: Quat::from_mat3(&frame),
        half_width: 32.0,
        half_height: 56.0,
        matrix: teleport_matrix((origin, angles), (partner.1, partner.2)),
    }
}

/// Blue on the floor at the origin, facing up; orange on a wall at x = 512,
/// facing back down the x axis.
fn floor_and_wall() -> (LinkedPortal, LinkedPortal) {
    let floor = (1, Vec3::ZERO, Vec3::new(-90.0, 0.0, 0.0));
    let wall = (2, Vec3::new(512.0, 0.0, 128.0), Vec3::new(0.0, 180.0, 0.0));
    (
        portal(floor.0, floor.1, floor.2, wall),
        portal(wall.0, wall.1, wall.2, floor),
    )
}

#[test]
fn only_a_floor_exit_has_a_minimum_speed_and_it_depends_on_the_entrance() {
    let (floor, wall) = floor_and_wall();
    let floor_to_floor = portal(3, Vec3::new(0.0, 256.0, 0.0), Vec3::new(-90.0, 0.0, 0.0), (1, floor.origin, Vec3::new(-90.0, 0.0, 0.0)));
    assert_eq!(exit_speed_range(&floor_to_floor, &floor), (225.0, 1000.0));
    assert_eq!(exit_speed_range(&wall, &floor), (50.0, 1000.0));
    let (minimum, maximum) = exit_speed_range(&floor, &wall);
    assert_eq!(minimum, f32::NEG_INFINITY);
    assert_eq!(maximum, 1000.0);
}

#[test]
fn an_exit_velocity_is_scaled_into_range_and_a_still_one_is_given_the_exit_forward() {
    let up = Vec3::Z;
    assert_eq!(clamp_exit_velocity(Vec3::ZERO, up, 50.0, 1000.0), up * 50.0);
    assert_eq!(clamp_exit_velocity(Vec3::ZERO, up, f32::NEG_INFINITY, 1000.0), Vec3::ZERO);
    // Scaled in the direction it was already going, not pushed along the
    // exit — the player's version adds, this one does not.
    let slow = clamp_exit_velocity(Vec3::new(10.0, 0.0, 0.0), up, 50.0, 1000.0);
    assert!((slow - Vec3::new(50.0, 0.0, 0.0)).length() < 1e-4, "{slow}");
    let fast = clamp_exit_velocity(Vec3::new(0.0, 0.0, -3000.0), up, 50.0, 1000.0);
    assert!((fast - Vec3::new(0.0, 0.0, -1000.0)).length() < 1e-3, "{fast}");
}

#[test]
fn the_held_side_toggles_the_way_valve_toggles_it() {
    let (floor, wall) = floor_and_wall();
    // The object goes in at the floor: it is now across the floor portal.
    let through = held_object_teleported(None, &floor);
    assert_eq!(through, Some(floor.id));
    // …and the player follows it in: both on the same side again.
    assert_eq!(player_teleported(through, &floor), None);
    // The player goes first: the object is across the portal the player came
    // *out* of.
    assert_eq!(player_teleported(None, &floor), Some(wall.id));
    // …and the object follows.
    assert_eq!(held_object_teleported(Some(wall.id), &floor), None);
}

#[test]
fn a_line_enters_a_portal_only_from_the_front_and_through_the_rectangle() {
    let (floor, _) = floor_and_wall();
    let t = floor.entered_by(Vec3::new(0.0, 0.0, 100.0), Vec3::new(0.0, 0.0, -100.0));
    assert_eq!(t, Some(0.5));
    // From behind is not in.
    assert_eq!(floor.entered_by(Vec3::new(0.0, 0.0, -100.0), Vec3::new(0.0, 0.0, 100.0)), None);
    // Through the plane beside the rectangle is not in: the portal's right is
    // world y here and its up is world x, half-widths 32 and 56.
    assert_eq!(floor.entered_by(Vec3::new(0.0, 40.0, 10.0), Vec3::new(0.0, 40.0, -10.0)), None);
    assert!(floor.entered_by(Vec3::new(50.0, 0.0, 10.0), Vec3::new(50.0, 0.0, -10.0)).is_some());
}

/// A world that is one infinite floor at z = 0 and nothing else.
struct Floor;

impl TouchQuery for Floor {
    fn brush_models_touching(&mut self, _: Vec3, _: Vec3, _: Vec3, _: Vec3, _: &mut Vec<usize>) {}

    fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
        false
    }

    fn solid_trace(&mut self, start: Vec3, end: Vec3, _: Vec3, _: Vec3) -> PushHit {
        if start.z >= 0.0 && end.z < 0.0 {
            let fraction = start.z / (start.z - end.z);
            return PushHit {
                fraction,
                end: start + (end - start) * fraction,
                start_solid: false,
            };
        }
        PushHit {
            fraction: 1.0,
            end,
            start_solid: start.z < 0.0,
        }
    }
}

#[test]
fn the_hold_trace_carries_on_through_a_portal_and_stops_at_the_floor_without_one() {
    let (floor, wall) = floor_and_wall();
    let (start, end) = (Vec3::new(0.0, 0.0, 50.0), Vec3::new(0.0, 0.0, -50.0));
    // No portal: the floor stops it half way.
    let blocked = trace_line_through(&mut Floor, &[], start, end);
    assert!((blocked - 0.5).abs() < 1e-5, "{blocked}");
    // Through the floor portal it comes out of the wall, into open air.
    let through = trace_line_through(&mut Floor, &[floor, wall], start, end);
    assert_eq!(through, 1.0);
}

// ---------------------------------------------------------------------------
// A cube and two portals, through the whole server
// ---------------------------------------------------------------------------

fn block(keys: &[(&str, &str)]) -> bsp::Entity {
    bsp::Entity {
        pairs: keys
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

fn box_solid(half: Vec3) -> CollideSolid {
    let points = (0..8)
        .map(|i| {
            Vec3::new(
                if i & 1 == 0 { -half.x } else { half.x },
                if i & 2 == 0 { -half.y } else { half.y },
                if i & 4 == 0 { -half.z } else { half.z },
            )
        })
        .collect();
    CollideSolid {
        mass_center: Vec3::ZERO,
        rotation_inertia: half * half * (2.0 / 3.0),
        radius: half.length(),
        ledges: vec![Ledge {
            points,
            triangles: Vec::new(),
            material: 0,
            mixed_materials: false,
        }],
    }
}

/// A floor slab `z ∈ [-64, 0]` and a wall `x ∈ [512, 576]`, both static, plus
/// the cube's model.
fn room() -> (Environment, HashMap<String, Model>) {
    let mut env = Environment::new(Default::default());
    for (half, at) in [
        (Vec3::new(1024.0, 1024.0, 32.0), Vec3::new(0.0, 0.0, -32.0)),
        (Vec3::new(32.0, 1024.0, 512.0), Vec3::new(544.0, 0.0, 512.0)),
    ] {
        env.add(Motion::Static, &Hulls::from_solid(&box_solid(half)), at, Vec3::ZERO, "default", None)
            .expect("a slab");
    }
    let solid = box_solid(Vec3::splat(17.8));
    let params = SolidParams {
        mass: 40.0,
        surface_prop: "metal".to_owned(),
        ..Default::default()
    };
    let mut models = HashMap::new();
    models.insert(
        "models/props/metal_box.mdl".to_owned(),
        Model {
            hulls: Hulls::from_solid(&solid),
            mass: Mass::from_solid(&solid, &params),
            jointed: false,
            params,
        },
    );
    (env, models)
}

/// A level with a cube at `cube`, two portals wired to a counter —
/// `OnEntityTeleportFromMe` adds 1, `OnEntityTeleportToMe` adds 10 — and the
/// room's physics.
fn level(cube: Vec3) -> (Server, EntityId) {
    let from_me = format!("count\u{1b}Add\u{1b}1\u{1b}0\u{1b}-1");
    let to_me = format!("count\u{1b}Add\u{1b}10\u{1b}0\u{1b}-1");
    let origin = format!("{} {} {}", cube.x, cube.y, cube.z);
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[("classname", "worldspawn")]),
            block(&[("classname", "math_counter"), ("targetname", "count")]),
            block(&[
                ("classname", "prop_portal"),
                ("targetname", "blue"),
                ("PortalTwo", "0"),
                ("OnEntityTeleportFromMe", &from_me),
                ("OnEntityTeleportToMe", &to_me),
            ]),
            block(&[
                ("classname", "prop_portal"),
                ("targetname", "orange"),
                ("PortalTwo", "1"),
                ("OnEntityTeleportFromMe", &from_me),
                ("OnEntityTeleportToMe", &to_me),
            ]),
            block(&[
                ("classname", "prop_weighted_cube"),
                ("targetname", "box"),
                ("origin", &origin),
            ]),
        ],
        &[],
    );
    let (env, models) = room();
    server.set_physics(env, models, Vec::new());
    let cube = name::find_by_name(&server.entities, "box").next().expect("the cube");
    (server, cube)
}

fn named(server: &Server, name: &str) -> EntityId {
    name::find_by_name(&server.entities, name).next().expect(name)
}

fn counter(server: &Server) -> f32 {
    let id = named(server, "count");
    server
        .entities
        .get(id)
        .and_then(|e| e.behaviour.downcast_ref::<MathCounter>())
        .expect("a counter")
        .value
}

fn run(server: &mut Server, ticks: usize) {
    for _ in 0..ticks {
        server.frame(1.0 / 64.0, &mut NoTouchQuery);
    }
}

const FLOOR: Vec3 = Vec3::new(-90.0, 0.0, 0.0);
const WALL_ORIGIN: Vec3 = Vec3::new(512.0, 0.0, 64.0);
const WALL: Vec3 = Vec3::new(0.0, 180.0, 0.0);

/// **The fling, in miniature.** A cube dropped onto a floor portal falls into
/// the floor — the slab under the portal does not hold it — comes out of the
/// wall portal moving away from the wall at the speed it fell, and lands on
/// the floor in front of it. Both portals' outputs fire once.
#[test]
fn a_cube_dropped_into_a_floor_portal_comes_out_of_the_wall_portal() {
    let (mut server, cube) = level(Vec3::new(0.0, 0.0, 200.0));
    assert!(server.place_portal(false, Vec3::ZERO, FLOOR));
    assert!(server.place_portal(true, WALL_ORIGIN, WALL));

    let mut out_at = None;
    for tick in 0..256 {
        run(&mut server, 1);
        let origin = server.entities.get(cube).unwrap().core.origin;
        assert!(origin.z > -40.0, "tick {tick}: the cube fell out of the world at {origin}");
        if out_at.is_none() && origin.x > 400.0 {
            out_at = Some((tick, origin));
        }
    }
    let (tick, at) = out_at.expect("the cube came out of the wall portal");
    assert!(at.x < 512.0, "out in front of the wall, not inside it: {at}");
    assert!((at.z - WALL_ORIGIN.z).abs() < 60.0, "out at the wall portal's height: {at}");
    assert!(tick < 64, "falling 200 units takes well under a second, not {tick} ticks");

    let rest = server.entities.get(cube).unwrap().core.origin;
    assert!(rest.x < 480.0 && rest.x > 60.0, "flung across the room, clear of both portals: {rest}");
    assert!((rest.z - 17.8).abs() < 2.0, "and lying on the floor: {rest}");
    // From the blue (+1), to the orange (+10), exactly once.
    assert_eq!(counter(&server), 11.0);
}

/// A floor portal with no partner is a hole in nothing: the cube lands on it
/// and stays, as it does in the shipped game — `ShouldTeleportTouchingEntity`
/// refuses without `m_hLinkedPortal`, and an unlinked portal carves nothing.
#[test]
fn a_cube_on_an_unlinked_floor_portal_rests_on_the_floor() {
    let (mut server, cube) = level(Vec3::new(0.0, 0.0, 200.0));
    assert!(server.place_portal(false, Vec3::ZERO, FLOOR));
    run(&mut server, 256);
    let rest = server.entities.get(cube).unwrap().core.origin;
    assert!((rest.z - 17.8).abs() < 1.0, "on the floor: {rest}");
    assert_eq!(counter(&server), 0.0);
}

/// **The far side of a portal's wall is still wall.** A portal on the
/// *underside* of the floor slab has its hole reaching up through the slab to
/// the floor's top face, where the cube lands — but the cube's centre is
/// behind that portal's plane, so the portal never takes it and the floor
/// holds it. Without the centre-in-front rule every cube on a floor over a
/// ceiling portal would fall through.
#[test]
fn a_cube_on_the_far_side_of_a_portals_wall_is_not_taken_by_it() {
    let (mut server, cube) = level(Vec3::new(0.0, 0.0, 200.0));
    assert!(server.place_portal(false, Vec3::new(0.0, 0.0, -64.0), Vec3::new(90.0, 0.0, 0.0)));
    assert!(server.place_portal(true, WALL_ORIGIN, WALL));
    run(&mut server, 256);
    let rest = server.entities.get(cube).unwrap().core.origin;
    assert!((rest.z - 17.8).abs() < 1.0, "still on the floor: {rest}");
    assert_eq!(counter(&server), 0.0);
}

fn player_at(origin: Vec3, yaw: f32) -> PlayerState {
    PlayerState {
        origin,
        angles: Vec3::new(0.0, yaw, 0.0),
        velocity: Vec3::ZERO,
        base_velocity: Vec3::ZERO,
        on_ground: true,
        move_type: crate::server::movement::MoveType::Walk,
        health: 100,
        life_state: Default::default(),
        flags: 0,
        buttons: 0,
        wish_velocity: Vec3::ZERO,
        vphysics_position: origin,
        teleported: false,
        portal_entered: None,
        view_offset: crate::client::player::VEC_VIEW,
        mins: Vec3::new(-16.0, -16.0, 0.0),
        maxs: Vec3::new(16.0, 16.0, 72.0),
    }
}

/// **Carrying a cube into a portal.** The player stands in front of the wall
/// portal holding the cube; the carry target is past the portal's plane, so
/// the hold trace goes through it and the cube is pushed in — and comes out
/// of the floor portal, where it is held, hovering over the hole, across the
/// pair. It stays held throughout.
#[test]
fn a_carried_cube_pushed_into_a_wall_portal_is_held_across_it() {
    let (mut server, cube) = level(Vec3::new(470.0, 0.0, 60.0));
    assert!(server.place_portal(false, Vec3::ZERO, FLOOR));
    assert!(server.place_portal(true, WALL_ORIGIN, WALL));
    // Facing the wall, close enough that the column puts the target behind it.
    server.spawn_player(player_at(Vec3::new(460.0, 0.0, 0.0), 0.0));
    run(&mut server, 1);
    server.pick_up(cube);
    assert_eq!(server.carried(), Some(cube), "picked up");

    run(&mut server, 128);
    assert_eq!(server.carried(), Some(cube), "still held");
    let orange = named(&server, "orange");
    assert_eq!(
        server.carry.and_then(|c| c.through),
        Some(orange),
        "held across the portal on the player's side"
    );
    let at = server.entities.get(cube).unwrap().core.origin;
    assert!(
        at.truncate().length() < 48.0 && at.z > 0.0 && at.z < 96.0,
        "the cube is out of the floor portal, above it: {at}"
    );
    assert_eq!(counter(&server), 11.0, "through once");

    // The player turns round: the target comes back out of the portal, and so
    // does the cube.
    let mut state = player_at(Vec3::new(460.0, 0.0, 0.0), 180.0);
    state.portal_entered = None;
    server.set_player_state(state);
    run(&mut server, 128);
    assert_eq!(server.carried(), Some(cube), "still held after turning");
    assert_eq!(server.carry.and_then(|c| c.through), None, "back on the player's side");
    let back = server.entities.get(cube).unwrap().core.origin;
    assert!(back.x < 460.0, "in front of the player again: {back}");
    assert_eq!(counter(&server), 22.0, "and back through once");
}

/// The matrix and the turn agree: a point and a direction taken through a
/// portal by [`LinkedPortal::matrix`] and by [`LinkedPortal::turn`] land in
/// the same place.
#[test]
fn the_turn_is_the_rotation_half_of_the_matrix() {
    let (floor, _) = floor_and_wall();
    let direction = Vec3::new(0.3, -0.2, -0.9).normalize();
    let by_matrix = floor.matrix.transform_vector3(direction);
    let by_turn = floor.turn() * direction;
    assert!((by_matrix - by_turn).length() < 1e-5);
    // Into the floor comes out of the wall, away from it.
    let out = floor.turn() * -Vec3::Z;
    assert!((out - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5, "{out}");
}

/// **Falling between two floor portals, on a shipped map.** `sp_a1_intro1`'s
/// cube is dropped and left to settle; then a portal is opened on the floor
/// under it and its partner on the floor 160 units off. The cube is asleep
/// when the portal opens — the portal has to wake it — and from then on it
/// falls into one, is thrown up out of the other at the floor-to-floor
/// minimum of 225, and falls back in.
///
/// **It is not endless, and that is the cube's doing, not the portals'.**
/// The floor here is a displacement, bumpy by a unit or two, and the cube
/// comes out of each pass tumbling and a little off centre — a floor-to-floor
/// pair mirrors the offset rather than cancelling it. Measured: four
/// crossings, then it lands across the rim of the blue portal and stays
/// there, its centre 12 units above the floor, which is what a cube does on
/// the edge of a hole. What the test holds to is that it went through more
/// than once and that no pass left it inside the floor.
///
/// Against the map's own brushes, displacements and props, which is what the
/// synthetic room above cannot say anything about: the world's collision here
/// is convex hulls built from the `.bsp`, not two boxes.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_cube_on_sp_a1_intro1_falls -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn the_cube_on_sp_a1_intro1_falls_between_two_floor_portals() {
    use crate::engine::trace::{CollisionBsp, Contents, Ray};
    use crate::engine::world::bsp::Bsp;
    use crate::engine::world::physics as world_physics;
    use crate::engine::world::props::Props;
    use crate::filesystem::Vfs;

    let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
        panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
    };
    let dir = std::path::PathBuf::from(dir);
    let base = dir.parent().unwrap_or(&dir).to_path_buf();
    let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");
    let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("load the map");
    let props = Props::load("sp_a1_intro1", &bsp).expect("the prop lump");
    let mut built = world_physics::build("sp_a1_intro1", &bsp, &props, &vfs, world_physics::surface_properties(&vfs));
    let mut server = Server::new();
    server.level_init("sp_a1_intro1", &bsp.entities(), &bsp.models);
    let names: Vec<String> = server.model_entities().into_iter().map(|e| e.model).collect();
    built.add_models(&names, &vfs);
    server.set_physics(built.environment, built.models, built.brush_models);
    let cube = named(&server, "box");

    // Let it drop out of its dropper and go to sleep.
    run(&mut server, 320);
    let rest = server.entities.get(cube).unwrap().core.origin;

    // The floor under a point, if it is flat floor within reach.
    let collision = CollisionBsp::build(&bsp);
    let floor_under = |at: Vec3| -> Option<Vec3> {
        let ray = Ray::line(at + Vec3::Z * 32.0, at - Vec3::Z * 96.0);
        let hit = collision.tracer().trace(&ray, Contents::MASK_SOLID);
        (hit.fraction < 1.0 && !hit.start_solid && hit.normal.z > 0.99).then_some(hit.end)
    };
    let blue = floor_under(rest).expect("the cube rests over flat floor");
    // The partner: the first flat floor 160 units away, in any of eight
    // directions, that is near enough level with the first. The chamber floor
    // here is a displacement, bumpy by a unit or two.
    let orange = (0..8)
        .filter_map(|i| {
            let yaw = (i as f32 * 45.0).to_radians();
            let at = rest + Vec3::new(yaw.cos(), yaw.sin(), 0.0) * 160.0;
            floor_under(at).filter(|p| (p.z - blue.z).abs() < 4.0)
        })
        .next()
        .expect("flat floor near the cube");
    eprintln!("cube at rest {rest}; blue {blue}, orange {orange}");

    assert!(server.place_portal(false, blue, FLOOR));
    assert!(server.place_portal(true, orange, FLOOR));

    let mut lowest = f32::MAX;
    let mut crossings = 0;
    let mut last_side = None;
    for _ in 0..640 {
        run(&mut server, 1);
        let at = server.entities.get(cube).unwrap().core.origin;
        lowest = lowest.min(at.z - blue.z);
        let side = (at.truncate() - blue.truncate()).length() < (at.truncate() - orange.truncate()).length();
        if last_side.is_some_and(|s| s != side) {
            crossings += 1;
        }
        last_side = Some(side);
    }
    eprintln!("{crossings} crossings in ten seconds; lowest centre {lowest:.1} relative to the floor");
    assert!(crossings >= 4, "the cube should keep falling between the two, not {crossings} times");
    assert!(lowest > -40.0, "the cube's centre went {lowest:.1} into the floor");
}
