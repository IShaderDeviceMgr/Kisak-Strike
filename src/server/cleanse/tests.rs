use std::collections::HashMap;

use glam::Vec3;

use super::*;
use crate::engine::world::bsp;
use crate::server::classes::MathCounter;
use crate::server::{movement, name, PlayerState, TouchQuery};
use crate::vphysics::collide::{Ledge, Solid as CollideSolid, SolidParams};
use crate::vphysics::env::{Environment, Hulls, Mass, Motion};
use crate::vphysics::Model;

fn block(pairs: &[(&str, &str)]) -> bsp::Entity {
    bsp::Entity {
        pairs: pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
    }
}

fn model(mins: Vec3, maxs: Vec3) -> bsp::Model {
    bsp::Model {
        mins: mins.to_array(),
        maxs: maxs.to_array(),
        origin: [0.0; 3],
        head_node: 0,
        first_face: 0,
        num_faces: 0,
    }
}

/// `*1`, the cleanser: a slab across the room at `x ∈ [-8, 8]`, `z` up to 128.
const FIELD_MINS: Vec3 = Vec3::new(-8.0, -128.0, 0.0);
const FIELD_MAXS: Vec3 = Vec3::new(8.0, 128.0, 128.0);

/// Brush models as boxes — the touch pass's question, answered for `*1`.
struct Field;

impl TouchQuery for Field {
    fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
        false
    }

    fn brush_models_touching(&mut self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, out: &mut Vec<usize>) {
        let (low, high) = (start.min(end) + mins, start.max(end) + maxs);
        if (0..3).all(|i| low[i] <= FIELD_MAXS[i] && high[i] >= FIELD_MINS[i]) {
            out.push(1);
        }
    }
}

fn connection(target: &str, input: &str, parameter: &str) -> String {
    format!("{target}\u{1b}{input}\u{1b}{parameter}\u{1b}0\u{1b}-1")
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

/// A floor at `z = 0` and the cube's model.
fn physics() -> (Environment, HashMap<String, Model>) {
    let mut env = Environment::new(Default::default());
    env.add(
        Motion::Static,
        &Hulls::from_solid(&box_solid(Vec3::new(1024.0, 1024.0, 32.0))),
        Vec3::new(0.0, 0.0, -32.0),
        Vec3::ZERO,
        "default",
        None,
    )
    .expect("a floor");
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

fn player_at(origin: Vec3) -> PlayerState {
    PlayerState {
        origin,
        angles: Vec3::ZERO,
        velocity: Vec3::ZERO,
        base_velocity: Vec3::ZERO,
        on_ground: true,
        move_type: movement::MoveType::Walk,
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

/// A level with a counter, a cleanser over `*1` wired to it — `OnFizzle`
/// adds 1, `OnDissolve` 10 — the `extra` blocks, physics and a player at
/// `x = -200`.
fn level(spawnflags: &str, cleanser_extra: &[(&str, &str)], extra: &[bsp::Entity]) -> Server {
    let on_fizzle = connection("count", "Add", "1");
    let on_dissolve = connection("count", "Add", "10");
    let mut cleanser = vec![
        ("classname", "trigger_portal_cleanser"),
        ("targetname", "fizzler"),
        ("model", "*1"),
        ("spawnflags", spawnflags),
        ("Visible", "1"),
        ("OnFizzle", on_fizzle.as_str()),
        ("OnDissolve", on_dissolve.as_str()),
    ];
    cleanser.extend_from_slice(cleanser_extra);
    let mut blocks = vec![
        block(&[("classname", "worldspawn")]),
        block(&[("classname", "math_counter"), ("targetname", "count")]),
        block(&cleanser),
    ];
    blocks.extend_from_slice(extra);
    let mut server = Server::new();
    server.level_init(
        "test",
        &blocks,
        &[model(Vec3::ZERO, Vec3::ZERO), model(FIELD_MINS, FIELD_MAXS)],
    );
    let (env, models) = physics();
    server.set_physics(env, models, Vec::new());
    server.spawn_player(player_at(Vec3::new(-200.0, 0.0, 0.0)));
    server
}

fn run(server: &mut Server, player: Vec3, ticks: usize) {
    for _ in 0..ticks {
        server.set_player_state(player_at(player));
        server.frame(1.0 / 64.0, &mut Field);
    }
}

fn counter(server: &Server) -> f32 {
    let id = name::find_by_name(&server.entities, "count").next().expect("the counter");
    server
        .entities
        .get(id)
        .and_then(|e| e.behaviour.downcast_ref::<MathCounter>())
        .expect("a counter")
        .value
}

/// Gun in hand and both portals up, well away from the field.
fn with_portals(server: &mut Server) {
    server.give_portalgun().expect("a player");
    server.upgrade_portalgun().expect("a gun");
    assert!(server.place_portal(false, Vec3::new(-300.0, 200.0, 64.0), Vec3::new(0.0, -90.0, 0.0)));
    assert!(server.place_portal(true, Vec3::new(-300.0, -200.0, 64.0), Vec3::new(0.0, 90.0, 0.0)));
    assert_eq!(server.portals().len(), 2);
}

/// **Walking through a fizzler closes your portals.** Both of them, whichever
/// side of the field they are on, and the cleanser says so once.
#[test]
fn a_player_walking_through_a_fizzler_loses_both_portals() {
    let mut server = level("4105", &[], &[]);
    with_portals(&mut server);
    run(&mut server, Vec3::new(-200.0, 0.0, 0.0), 2);
    assert_eq!(server.portals().len(), 2, "not in the field yet");
    run(&mut server, Vec3::ZERO, 4);
    assert!(server.portals().is_empty(), "both portals fizzled");
    assert_eq!(counter(&server), 1.0, "OnFizzle once, not once a tick");
    assert_eq!(server.portalgun().map(|g| g.last_fired_portal), Some(0));
}

/// Spawnflags 4104 — physics objects only — lets the player through with
/// their portals, and a disabled field does nothing at all.
#[test]
fn a_physics_only_or_disabled_fizzler_leaves_the_players_portals() {
    let mut server = level("4104", &[], &[]);
    with_portals(&mut server);
    run(&mut server, Vec3::ZERO, 4);
    assert_eq!(server.portals().len(), 2, "4104 does not take clients");

    let mut server = level("4105", &[("StartDisabled", "1")], &[]);
    with_portals(&mut server);
    run(&mut server, Vec3::ZERO, 4);
    assert_eq!(server.portals().len(), 2, "disabled");
    assert_eq!(counter(&server), 0.0);
}

/// **A cube dropped into a fizzler is dissolved**: its own `OnFizzled`
/// (100 here), then the cleanser's `OnDissolve` (10), and it is gone.
#[test]
fn a_cube_falling_into_a_fizzler_is_dissolved() {
    let fizzled = connection("count", "Add", "100");
    let cube = block(&[
        ("classname", "prop_weighted_cube"),
        ("targetname", "box"),
        ("origin", "0 0 200"),
        ("OnFizzled", fizzled.as_str()),
    ]);
    let mut server = level("4105", &[], &[cube]);
    run(&mut server, Vec3::new(-200.0, 0.0, 0.0), 128);
    assert!(
        name::find_by_name(&server.entities, "box").next().is_none(),
        "the cube is gone"
    );
    assert_eq!(counter(&server), 110.0);
}

/// Clients only (4097): the same cube falls through the field and lands.
#[test]
fn a_clients_only_fizzler_lets_a_cube_through() {
    let cube = block(&[("classname", "prop_weighted_cube"), ("targetname", "box"), ("origin", "0 0 200")]);
    let mut server = level("4097", &[], &[cube]);
    run(&mut server, Vec3::new(-200.0, 0.0, 0.0), 128);
    let id = name::find_by_name(&server.entities, "box").next().expect("the cube survived");
    let z = server.entities.get(id).unwrap().core.origin.z;
    assert!((z - 17.8).abs() < 1.0, "on the floor, at {z}");
    assert_eq!(counter(&server), 0.0);
}

/// `FizzleTouchingPortals`: the portal inside the field goes, the one outside
/// stays.
#[test]
fn fizzle_touching_portals_takes_only_the_portal_in_the_field() {
    let mut server = level("4105", &[], &[]);
    server.give_portalgun().expect("a player");
    server.upgrade_portalgun().expect("a gun");
    assert!(server.place_portal(false, Vec3::new(0.0, 0.0, 64.0), Vec3::new(0.0, 0.0, 0.0)));
    assert!(server.place_portal(true, Vec3::new(-300.0, -200.0, 64.0), Vec3::new(0.0, 90.0, 0.0)));
    let fizzler = name::find_by_name(&server.entities, "fizzler").next().unwrap();
    server.accept_input(fizzler, "FizzleTouchingPortals", Variant::Void, None, None, 0);
    let left = server.portals();
    assert_eq!(left.len(), 1);
    assert!(left[0].is_portal2, "the orange one, outside the field, is still up");
}

/// **The dropper loop.** A cube made by an `env_entity_maker` falls into the
/// field; its `OnFizzled` triggers a relay that fires `ForceSpawn` at the
/// maker again; a new cube appears. After three rounds, three cubes have been
/// dissolved and a fourth is falling.
#[test]
fn a_dissolved_cube_is_replaced_by_its_dropper() {
    let respawn = connection("respawn", "Trigger", "");
    let spawn = connection("maker", "ForceSpawn", "");
    let counted = connection("count", "Add", "100");
    let blocks = [
        block(&[
            ("classname", "prop_weighted_cube"),
            ("targetname", "dropped"),
            ("origin", "0 0 200"),
            ("OnFizzled", respawn.as_str()),
            ("OnFizzled", counted.as_str()),
        ]),
        block(&[
            ("classname", "point_template"),
            ("targetname", "cube_template"),
            ("spawnflags", "2"),
            ("origin", "0 0 200"),
            ("Template01", "dropped"),
        ]),
        block(&[
            ("classname", "env_entity_maker"),
            ("targetname", "maker"),
            ("origin", "0 0 200"),
            ("EntityTemplate", "cube_template"),
        ]),
        block(&[("classname", "logic_relay"), ("targetname", "respawn"), ("OnTrigger", spawn.as_str())]),
    ];
    let mut server = level("4105", &[], &blocks);
    assert!(
        name::find_by_name(&server.entities, "dropped").next().is_none(),
        "a template's cube is not in the map until it is made"
    );
    let maker = name::find_by_name(&server.entities, "maker").next().unwrap();
    server.accept_input(maker, "ForceSpawn", Variant::Void, None, None, 0);
    assert!(name::find_by_name(&server.entities, "dropped").next().is_some(), "made");

    run(&mut server, Vec3::new(-200.0, 0.0, 0.0), 3 * 64);
    assert!(
        counter(&server) >= 300.0,
        "at least three cubes fizzled and each was replaced, counter {}",
        counter(&server)
    );
    assert_eq!(
        name::find_by_name(&server.entities, "dropped").count(),
        1,
        "and there is always exactly one"
    );
}
