//! The portal gun on a whole server: how the player gets one, what the
//! commands do, and a shot from `+attack` to a portal on the wall.
//!
//! The world is the placement tests' standard room — a wall whose face is
//! `x = 0` — and the player stands at `x = -256` with their eye at `z = 128`,
//! looking along `+x`.

use glam::Vec3;

use super::*;
use crate::engine::world::bsp;
use crate::server::io::{Event, Target};
use crate::server::placement::tests::{wall, world, FixtureWorld};
use crate::server::{PlayerState, Server};

fn block(pairs: &[(&str, &str)]) -> bsp::Entity {
    bsp::Entity {
        pairs: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

/// The player at `origin`, facing `+x`, holding `buttons`.
fn player(origin: Vec3, buttons: u32) -> PlayerState {
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
        buttons,
        wish_velocity: Vec3::ZERO,
        vphysics_position: origin,
        teleported: false,
        portal_entered: None,
        view_offset: crate::client::player::VEC_VIEW,
        mins: Vec3::new(-16.0, -16.0, 0.0),
        maxs: Vec3::new(16.0, 16.0, 72.0),
    }
}

/// Where the player stands: eye at `(-256, y, 128)`.
fn feet(y: f32) -> Vec3 {
    Vec3::new(-256.0, y, 64.0)
}

/// A level called `map` with nothing in it, and a player.
fn level(map: &str, blocks: &[bsp::Entity]) -> Server {
    let mut server = Server::new();
    let mut all = vec![block(&[("classname", "worldspawn")])];
    all.extend_from_slice(blocks);
    server.level_init(map, &all, &[]);
    server.spawn_player(player(feet(0.0), 0));
    server
}

/// One tick with the player at `feet(y)` holding `buttons`.
fn tick(server: &mut Server, world: &mut FixtureWorld, y: f32, buttons: u32) {
    server.set_player_state(player(feet(y), buttons));
    let interval = server.time().interval;
    server.frame(interval, world);
}

fn active_portals(server: &Server) -> Vec<crate::server::PortalState> {
    server.portals()
}

fn guns(server: &Server) -> usize {
    server
        .entities
        .iter()
        .filter(|(_, e)| e.core.class.name == "weapon_portalgun" && !e.core.removed)
        .count()
}

#[test]
fn give_portalgun_needs_a_player() {
    let mut server = Server::new();
    server.level_init("test", &[block(&[("classname", "worldspawn")])], &[]);
    assert!(server.give_portalgun().is_err());
    assert!(server.upgrade_portalgun().is_err());
}

/// The transition script's three commands, and why each has to be
/// idempotent: 60 single-player maps have two `@command`s, so every command
/// arrives twice.
#[test]
fn the_three_commands_give_blue_then_orange_then_the_potato() {
    let mut server = level("test", &[]);
    server.give_portalgun().expect("a player");
    let gun = server.portalgun().expect("given");
    assert!(gun.can_fire_portal1 && !gun.can_fire_portal2, "a blue gun");

    server.give_portalgun().expect("again");
    assert_eq!(guns(&server), 1, "the second give makes nothing");

    server.upgrade_portalgun().expect("a gun");
    let gun = server.portalgun().expect("still");
    assert!(gun.can_fire_portal1 && gun.can_fire_portal2 && !gun.potato);

    server.upgrade_potatogun().expect("a gun");
    let gun = server.portalgun().expect("still");
    assert!(gun.can_fire_portal2 && gun.potato, "orange, and the potato");
}

/// `give weapon_portalgun` is a blue gun — the constructor's default — except
/// on the one map `BumpWeapon` names.
#[test]
fn give_weapon_portalgun_is_blue_except_in_the_incinerator() {
    let mut server = level("sp_a1_intro3", &[]);
    server.give_named_item("weapon_portalgun").expect("a class");
    let gun = server.portalgun().expect("given");
    assert!(gun.can_fire_portal1 && !gun.can_fire_portal2);

    let mut server = level("sp_a2_intro", &[]);
    server.give_named_item("weapon_portalgun").expect("a class");
    let gun = server.portalgun().expect("given");
    assert!(gun.can_fire_portal1 && gun.can_fire_portal2, "the incinerator hack");

    assert!(server.give_named_item("weapon_not_a_thing").is_err());
}

/// `sp_a3_01`'s gun, on the floor: walking into it picks it up, with the
/// chips the map wrote, and it stops being drawn.
#[test]
fn a_gun_on_the_floor_is_picked_up_by_walking_into_it() {
    let mut server = level(
        "test",
        &[
            block(&[
                ("classname", "weapon_portalgun"),
                ("targetname", "knockout-portalgun"),
                ("origin", "-256 0 64"),
                ("CanFirePortal1", "1"),
                ("CanFirePortal2", "1"),
                ("spawnflags", "1"),
            ]),
            block(&[("classname", "logic_relay"), ("targetname", "picked")]),
        ],
    );
    assert!(server.portalgun().is_none());
    let mut w = world(wall);
    tick(&mut server, &mut w, 0.0, 0);
    let gun = server.portalgun().expect("picked up");
    assert!(gun.can_fire_portal1 && gun.can_fire_portal2);
    let id = server.player_portalgun().expect("owned");
    let entity = server.entities.get(id).expect("alive");
    assert!(entity.core.effects & movement::EF_NODRAW != 0, "not drawn once carried");
    assert!(!entity.core.is_solid_flag_set(movement::FSOLID_TRIGGER));
}

/// The two command classes, fired the way `sp_transition_list.nut` fires
/// them: both are named `@command`, so one `EntFire` reaches both.
#[test]
fn both_command_entities_pass_their_command_on() {
    let mut server = level(
        "test",
        &[
            block(&[("classname", "point_servercommand"), ("targetname", "@command")]),
            block(&[("classname", "point_clientcommand"), ("targetname", "@command")]),
        ],
    );
    server.queue.add(Event {
        fire_time: 0.0,
        target: Target::Name(String::from("@command")),
        input: String::from("Command"),
        value: Variant::String(String::from("give_portalgun")),
        activator: None,
        caller: None,
        output_id: 0,
    });
    let mut w = world(wall);
    tick(&mut server, &mut w, 0.0, 0);
    assert_eq!(server.take_server_commands(), vec!["give_portalgun".to_owned()]);
    assert_eq!(server.take_console_commands(), vec!["give_portalgun".to_owned()]);
}

/// `+attack` with a blue gun puts a blue portal where the player is looking;
/// `+attack2` does nothing until the gun is upgraded, and then the second
/// portal links to the first.
#[test]
fn attack_fires_blue_and_attack2_fires_orange_once_upgraded() {
    let mut server = level("test", &[]);
    let mut w = world(wall);
    server.give_portalgun().expect("a player");
    // `SetCanFirePortal1` holds a freshly chipped gun for a quarter of a
    // second — so the first click after `give_portalgun` does nothing.
    tick(&mut server, &mut w, 0.0, IN_ATTACK);
    assert!(active_portals(&server).is_empty(), "held by the upgrade delay");
    for _ in 0..20 {
        tick(&mut server, &mut w, 0.0, 0);
    }

    tick(&mut server, &mut w, 0.0, IN_ATTACK);
    let portals = active_portals(&server);
    assert_eq!(portals.len(), 1, "{portals:?}");
    assert!(!portals[0].is_portal2);
    assert!((portals[0].origin - Vec3::new(-0.03125, 0.0, 128.0)).abs().max_element() < 0.01);
    assert!(portals[0].linked.is_none());
    assert_eq!(server.portalgun().unwrap().last_fired_portal, 1);

    // No orange chip: the right button does nothing.
    tick(&mut server, &mut w, 0.0, 0);
    for _ in 0..20 {
        tick(&mut server, &mut w, 100.0, IN_ATTACK2);
    }
    assert_eq!(active_portals(&server).len(), 1);

    server.upgrade_portalgun().expect("a gun");
    tick(&mut server, &mut w, 100.0, 0);
    for _ in 0..40 {
        tick(&mut server, &mut w, 100.0, IN_ATTACK2);
    }
    let portals = active_portals(&server);
    assert_eq!(portals.len(), 2, "{portals:?}");
    let orange = portals.iter().find(|p| p.is_portal2).expect("orange");
    // 100 is 28 from the wall's edge: bumped in until flush.
    assert!((orange.origin.y - 96.0).abs() < 0.1, "{orange:?}");
    assert!(portals.iter().all(|p| p.linked.is_some()), "linked: {portals:?}");
}

/// A held button fires every **half** second; a clicked one every fifth of
/// one. `m_afButtonLast` is what tells them apart.
#[test]
fn a_held_button_repeats_slower_than_a_clicked_one() {
    let shots = |server: &mut Server, w: &mut FixtureWorld, pattern: &dyn Fn(u32) -> u32| {
        let mut count = 0;
        let mut last = server.view_model().map(|v| v.started_at);
        for t in 0..64 {
            tick(server, w, 0.0, pattern(t));
            let now = server.view_model().map(|v| (v.sequence, v.started_at));
            if let Some(("fire1", started)) = now {
                if Some(started) != last {
                    count += 1;
                    last = Some(started);
                }
            }
        }
        count
    };

    let mut server = level("test", &[]);
    let mut w = world(wall);
    server.give_portalgun().expect("a player");
    for _ in 0..20 {
        tick(&mut server, &mut w, 0.0, 0);
    }
    // One second at 64 Hz, held throughout: 0, 0.5 — and not 1.0, which is
    // the 65th tick.
    let held = shots(&mut server, &mut w, &|_| IN_ATTACK);
    assert_eq!(held, 2);

    for _ in 0..40 {
        tick(&mut server, &mut w, 0.0, 0);
    }
    // Pressed on alternate ticks: every 0.2 s once the delay allows, which
    // over a second is five or six presses depending on where the ticks fall.
    let clicked = shots(&mut server, &mut w, &|t| if t % 2 == 0 { IN_ATTACK } else { 0 });
    assert!((5..=6).contains(&clicked), "{clicked}");
}

/// A fizzler between the gun and the wall stops the shot while it is on —
/// and is drawn while it is on — and neither once it is switched off.
#[test]
fn a_fizzler_blocks_shots_and_shows_only_while_it_is_enabled() {
    let fizzler = block(&[
        ("classname", "trigger_portal_cleanser"),
        ("targetname", "fizzler"),
        ("model", "*1"),
        ("Visible", "1"),
        ("StartDisabled", "0"),
        ("spawnflags", "4105"),
    ]);
    let mut server = Server::new();
    let models = [
        bsp::Model {
            mins: [-32768.0; 3],
            maxs: [32767.0; 3],
            origin: [0.0; 3],
            head_node: 0,
            first_face: 0,
            num_faces: 0,
        },
        bsp::Model {
            mins: [-104.0, -128.0, 0.0],
            maxs: [-96.0, 128.0, 256.0],
            origin: [0.0; 3],
            head_node: 0,
            first_face: 0,
            num_faces: 0,
        },
    ];
    server.level_init("test", &[block(&[("classname", "worldspawn")]), fizzler], &models);
    server.spawn_player(player(feet(0.0), 0));
    server.give_portalgun().expect("a player");
    let mut w = world(wall);
    let id = crate::server::name::find_by_name(&server.entities, "fizzler").next().expect("spawned");
    let drawn = |server: &Server| server.entities.get(id).unwrap().core.effects & movement::EF_NODRAW == 0;
    assert!(drawn(&server), "a visible, enabled fizzler draws its field");

    let gun = server.player_portalgun().expect("given");
    let shot = server.fire_portal(gun, false, &mut w).expect("fired");
    assert_eq!(shot.result, PlacementResult::Cleanser);
    assert!(active_portals(&server).is_empty());

    server.queue.add(Event {
        fire_time: 0.0,
        target: Target::Name(String::from("fizzler")),
        input: String::from("Disable"),
        value: Variant::Void,
        activator: None,
        caller: None,
        output_id: 0,
    });
    tick(&mut server, &mut w, 0.0, 0);
    assert!(!drawn(&server), "a disabled fizzler's field is gone");
    let shot = server.fire_portal(gun, false, &mut w).expect("fired");
    assert!(shot.result.succeeded(), "{shot:?}");
    assert_eq!(active_portals(&server).len(), 1);
}
