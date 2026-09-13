//! The spawn pipeline and the I/O loop, against synthetic maps and against the
//! 106 real ones.
//!
//! Nothing here needs a GPU, which is the point of `portdocs/SERVER.md` §3 —
//! the entity system is text in and a list out.

use super::*;
use crate::engine::world::bsp;

/// One entity-lump block.
fn block(pairs: &[(&str, &str)]) -> bsp::Entity {
    bsp::Entity {
        pairs: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

/// A connection value, as the lump spells one.
fn conn(target: &str, input: &str, parameter: &str, delay: &str, times: &str) -> String {
    format!("{target}\u{1b}{input}\u{1b}{parameter}\u{1b}{delay}\u{1b}{times}")
}

/// A small map with one of nearly everything stage 1 knows.
fn sample() -> Vec<bsp::Entity> {
    vec![
        block(&[
            ("classname", "worldspawn"),
            ("skyname", "sky_day01_01"),
            ("world_mins", "-3104 -2048 -192"),
            ("world_maxs", "4096 8120 250"),
            ("maxblobcount", "250"),
            ("detailvbsp", "detail.vbsp"),
            ("hammerid", "1"),
        ]),
        // Baked by `vrad` and deleted on spawn.
        block(&[
            ("classname", "light"),
            ("origin", "0 0 128"),
            ("_light", "255 255 255 200"),
            ("_quadratic_attn", "1"),
        ]),
        // Named, so it survives.
        block(&[
            ("classname", "light_spot"),
            ("targetname", "flicker"),
            ("origin", "64 0 128"),
            ("angles", "0 90 0"),
            ("pitch", "-45"),
            ("style", "33"),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "start_relay"),
            ("StartDisabled", "1"),
            ("spawnflags", "2"),
            ("OnTrigger", &conn("door", "Open", "", "0", "-1")),
            ("OnTrigger", &conn("panel", "Close", "", "1.5", "1")),
            ("OnUser1", &conn("other", "Kill", "", "0", "-1")),
            ("OnUnPressed", &conn("junk", "Kill", "", "0", "-1")),
        ]),
        block(&[
            ("classname", "func_instance_io_proxy"),
            ("targetname", "proxy"),
            (
                "OnProxyRelay1",
                &conn("start_relay", "Trigger", "", "0", "-1"),
            ),
            ("OnProxyRelay", &conn("unfixed", "Trigger", "", "0", "-1")),
        ]),
        block(&[
            ("classname", "info_target"),
            ("targetname", "child"),
            ("parentname", "flicker"),
            ("origin", "1 2 3"),
        ]),
        block(&[("classname", "prop_dynamic"), ("model", "models/x.mdl")]),
        block(&[("classname", "prop_dynamic"), ("model", "models/y.mdl")]),
    ]
}

#[test]
fn a_map_becomes_an_entity_list() {
    let mut server = Server::new();
    let stats = server.level_init("test", &sample());

    assert_eq!(stats.blocks, 8);
    assert_eq!(stats.matched, 6, "prop_dynamic is not implemented yet");
    assert_eq!(stats.unknown.get("prop_dynamic"), Some(&2));
    assert_eq!(
        stats.removed_on_spawn, 1,
        "the unnamed light removes itself and nothing else does"
    );
    assert_eq!(stats.spawned, 5);
    assert_eq!(server.entities.len(), 5);

    // Two `OnTrigger`s, an `OnUser1`, and one of the proxy's thirty. The
    // unnumbered `OnProxyRelay` and the mapper's `OnUnPressed` are not
    // outputs of anything, so they are unhandled — which is what the shipped
    // server does with them too.
    assert_eq!(stats.outputs, 4);
    assert_eq!(stats.unhandled.get("onproxyrelay"), Some(&1));
    assert_eq!(stats.unhandled.get("onunpressed"), Some(&1));
    assert_eq!(stats.unhandled.get("_quadratic_attn"), Some(&1));
    assert_eq!(stats.unhandled.get("detailvbsp"), Some(&1));
    assert_eq!(
        stats.unhandled.get("_light"),
        Some(&1),
        "on the deleted light"
    );

    assert_eq!(stats.parented, 1);
    assert_eq!(stats.parents_missing, 0);
}

#[test]
fn the_keys_land_where_the_class_says_they_do() {
    let mut server = Server::new();
    server.level_init("test", &sample());

    let world = find(&server, "worldspawn");
    let world = world
        .behaviour
        .downcast_ref::<classes::World>()
        .expect("worldspawn is a World");
    assert_eq!(world.sky_name.as_deref(), Some("sky_day01_01"));
    assert_eq!(world.world_maxs, glam::Vec3::new(4096.0, 8120.0, 250.0));
    assert_eq!(world.max_blob_count, 250);

    let light = find(&server, "light_spot");
    // `angles` then `pitch`, in lump order, so the pitch survives — the
    // ordering every `light_spot` in the shipped game relies on.
    assert_eq!(light.angles, glam::Vec3::new(-45.0, 90.0, 0.0));
    assert_eq!(
        light
            .behaviour
            .downcast_ref::<classes::Light>()
            .expect("a Light")
            .style,
        33
    );

    let relay = find(&server, "logic_relay");
    assert_eq!(relay.spawn_flags, 2);
    assert!(
        relay
            .behaviour
            .downcast_ref::<classes::Relay>()
            .expect("a Relay")
            .disabled
    );
    assert_eq!(relay.output_count("OnTrigger"), 2);
    assert_eq!(relay.output_count("OnUser1"), 1);
    assert_eq!(relay.outputs.len(), 2, "two names, three connections");
}

fn find<'a>(server: &'a Server, classname: &str) -> &'a Entity {
    server
        .entities
        .iter()
        .map(|(_, e)| e)
        .find(|e| e.classname() == classname)
        .unwrap_or_else(|| panic!("no {classname} in the list"))
}

fn find_named<'a>(server: &'a Server, targetname: &str) -> &'a Entity {
    let id = name::find_by_name(&server.entities, targetname)
        .next()
        .unwrap_or_else(|| panic!("no entity named {targetname}"));
    server.entities.get(id).expect("just found")
}

/// `mapentities.cpp:373` — worldspawn is spawned outside the sorted list and
/// is not allowed a parent, whatever the lump says.
#[test]
fn worldspawn_is_never_parented() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[("classname", "info_target"), ("targetname", "anchor")]),
            block(&[("classname", "worldspawn"), ("parentname", "anchor")]),
        ],
    );
    let world = find(&server, "worldspawn");
    assert!(world.parent_name.is_none());
    assert!(world.parent.is_none());
}

/// `ComputeSpawnHierarchyDepth` plus the sort: a parent spawns before its
/// child even when the lump lists the child first.
#[test]
fn a_child_spawns_after_its_parent_whatever_the_lump_order() {
    let mut server = Server::new();
    // Deliberately deepest first.
    server.level_init(
        "test",
        &[
            block(&[
                ("classname", "info_target"),
                ("targetname", "grandchild"),
                ("parentname", "child"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "child"),
                ("parentname", "root"),
            ]),
            block(&[("classname", "info_target"), ("targetname", "root")]),
        ],
    );

    let by_name = |name: &str| {
        name::find_by_name(&server.entities, name)
            .next()
            .unwrap_or_else(|| panic!("no {name}"))
    };
    let (root, child, grandchild) = (by_name("root"), by_name("child"), by_name("grandchild"));
    assert_eq!(server.spawn_hierarchy_depth(root), 1);
    assert_eq!(server.spawn_hierarchy_depth(child), 2);
    assert_eq!(server.spawn_hierarchy_depth(grandchild), 3);

    let order = server.spawn_order(&[grandchild, child, root]);
    assert_eq!(order, vec![root, child, grandchild]);

    // And the handles are resolved, not just the names.
    assert_eq!(server.entities.get(child).unwrap().parent, Some(root));
    assert_eq!(server.entities.get(grandchild).unwrap().parent, Some(child));
}

/// A `parentname` naming nothing is not an error — the entity is simply at
/// depth 1 and unparented, and the count is reported.
#[test]
fn a_missing_parent_is_counted_and_not_fatal() {
    let mut server = Server::new();
    let stats = server.level_init(
        "test",
        &[block(&[
            ("classname", "info_target"),
            ("targetname", "orphan"),
            ("parentname", "nobody"),
        ])],
    );
    assert_eq!((stats.parented, stats.parents_missing), (1, 1));
    assert!(find(&server, "info_target").parent.is_none());
}

/// Valve checks only for an entity parented to itself; a longer cycle recurses
/// until the stack runs out. Both terminate here.
#[test]
fn a_parent_cycle_terminates() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[
                ("classname", "info_target"),
                ("targetname", "a"),
                ("parentname", "b"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "b"),
                ("parentname", "a"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "self"),
                ("parentname", "self"),
            ]),
        ],
    );
    assert_eq!(server.entities.len(), 3);
}

/// The priority table is data with no reachable entry yet; this is what stops
/// it rotting before `prop_physics` arrives to need it.
#[test]
fn spawn_priority_is_valves_table() {
    assert_eq!(spawn_priority("func_wall"), 10);
    assert_eq!(spawn_priority("FUNC_WALL"), 10);
    assert_eq!(spawn_priority("prop_physics"), 7);
    assert_eq!(spawn_priority("phys_ballsocket"), 8);
    assert_eq!(spawn_priority("logic_relay"), -1);
    // None of the thirteen is implemented, so none of them can fire today.
    for (name, _) in SPAWN_PRIORITY {
        assert!(
            classes::lookup(name).is_none(),
            "{name} is registered now — the priority table is live, \
             and this assertion should become a real ordering test"
        );
    }
}

#[test]
fn a_second_level_replaces_the_first_and_shutdown_empties_the_list() {
    let mut server = Server::new();
    server.level_init("first", &sample());
    assert_eq!(server.entities.len(), 5);

    let stats = server.level_init("second", &[block(&[("classname", "info_target")])]);
    assert_eq!(stats.blocks, 1);
    assert_eq!(server.entities.len(), 1);
    assert_eq!(server.map.as_deref(), Some("second"));

    server.level_shutdown();
    assert!(server.entities.is_empty());
    assert!(server.map.is_none());
    assert_eq!(server.queue.len(), 0);
    assert_eq!(server.time().tick, 0, "the clock restarts with the level");
    server.level_shutdown();
}

/// A block with no `classname` is skipped, the way `MapEntity_ParseEntity`
/// skips it, and does not take the rest of the lump with it.
#[test]
fn a_block_with_no_classname_is_skipped() {
    let mut server = Server::new();
    let stats = server.level_init(
        "test",
        &[
            block(&[("origin", "0 0 0")]),
            block(&[("classname", "info_target")]),
        ],
    );
    assert_eq!(stats.blocks, 2);
    assert_eq!(stats.matched, 1);
    assert_eq!(stats.unknown.get("<no classname>"), Some(&1));
}

// ---------------------------------------------------------------------------
// stage 2: entity I/O, the queue and thinks
// ---------------------------------------------------------------------------

/// Runs `seconds` of server time in whole ticks.
fn run(server: &mut Server, seconds: f32) {
    let interval = server.time().interval;
    let ticks = (seconds / interval).round() as u32;
    for _ in 0..ticks {
        server.frame(interval);
    }
}

/// A `math_counter` is the cheapest observable sink in the game: fire `Add` at
/// it and read the number back.
fn counter_value(server: &Server, name: &str) -> f32 {
    find_named(server, name)
        .behaviour
        .downcast_ref::<classes::MathCounter>()
        .expect("a MathCounter")
        .value
}

/// The whole loop end to end: a relay's `OnSpawn` starts a chain that reaches
/// a counter, through the queue, on a think, in one tick.
#[test]
fn a_relay_fires_its_outputs_and_they_reach_their_target() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[
                ("classname", "logic_relay"),
                ("targetname", "starter"),
                ("OnSpawn", &conn("count", "Add", "5", "0", "-1")),
            ]),
            block(&[
                ("classname", "math_counter"),
                ("targetname", "count"),
                ("max", "100"),
            ]),
        ],
    );

    assert_eq!(counter_value(&server, "count"), 0.0);
    // `Activate` scheduled the think for curtime + 0.01, which is one tick at
    // 64 Hz — so a single tick runs the think, the queue and the input.
    run(&mut server, 0.02);
    assert_eq!(counter_value(&server, "count"), 5.0);
    assert_eq!(server.io.thinks, 1);
    assert_eq!(server.io.accepted, 1);
}

/// > **The queue restarts from the head after every event**, so a chain of
/// > zero-delay relays completes within one `ServiceEvents` call. Get this
/// > wrong and every map in the game runs its logic in slow motion.
#[test]
fn a_zero_delay_chain_completes_in_one_tick() {
    let mut relays: Vec<bsp::Entity> = (0..8)
        .map(|i| {
            block(&[
                ("classname", "logic_relay"),
                ("targetname", &format!("relay{i}")),
                (
                    "OnTrigger",
                    &match i {
                        7 => conn("count", "Add", "1", "0", "-1"),
                        _ => conn(&format!("relay{}", i + 1), "Trigger", "", "0", "-1"),
                    },
                ),
            ])
        })
        .collect();
    relays.push(block(&[
        ("classname", "math_counter"),
        ("targetname", "count"),
        ("max", "100"),
    ]));
    // The thing that starts it: an `OnSpawn` on the first relay.
    relays[0].pairs.push((
        String::from("OnSpawn"),
        conn("relay0", "Trigger", "", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &relays);

    // One tick. The think fires `OnSpawn`, and the eight-deep chain drains
    // inside the same `service_events`.
    run(&mut server, 0.02);
    assert_eq!(counter_value(&server, "count"), 1.0);
    assert_eq!(server.io.thinks, 1, "one think, not eight");

    // What *is* left over is one `EnableRefire` per relay: none of the eight
    // set `SF_ALLOW_FAST_RETRIGGER`, so each latched itself on the way through
    // and posted its own unlatch a millisecond later. Eight relays, eight
    // latches, and the chain still finished inside the one tick.
    assert_eq!(server.queue.len(), 8);
    assert!(server
        .queue
        .iter()
        .all(|event| event.input == "EnableRefire"));
}

/// `logic_auto` is how every map in the game starts itself, and the 0.2 second
/// delay is the part that is easy to leave out.
#[test]
fn logic_auto_fires_on_map_spawn_after_two_tenths_of_a_second() {
    let map = vec![
        block(&[
            ("classname", "logic_auto"),
            ("spawnflags", "1"),
            ("OnMapSpawn", &conn("count", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    assert_eq!(server.entities.len(), 2);

    run(&mut server, 0.1);
    assert_eq!(counter_value(&server, "count"), 0.0, "not yet");
    run(&mut server, 0.15);
    assert_eq!(counter_value(&server, "count"), 1.0);

    // `SF_AUTO_FIREONCE`: 998 of the game's 1,112 remove themselves here.
    assert_eq!(server.entities.len(), 1, "the logic_auto deleted itself");

    // …and it does not fire twice.
    run(&mut server, 1.0);
    assert_eq!(counter_value(&server, "count"), 1.0);
}

/// The refire latch. Without it a relay re-triggered during its own delay
/// double-fires, and 86% of the game's relays rely on it.
#[test]
fn a_relay_latches_until_its_slowest_output_has_gone_out() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "gate"),
            ("OnTrigger", &conn("count", "Add", "1", "0.5", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "pusher"),
            ("OnSpawn", &conn("gate", "Trigger", "", "0", "-1")),
            // A second trigger, a tenth of a second later — inside the gate's
            // half-second delay, so the latch must refuse it.
            ("OnSpawn", &conn("gate", "Trigger", "", "0.1", "-1")),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    run(&mut server, 1.0);
    assert_eq!(
        counter_value(&server, "count"),
        1.0,
        "the second Trigger arrived while latched and must have been refused"
    );

    // Once the latch has lifted — `GetMaxDelay() + 0.001` — it works again.
    let id = name::find_by_name(&server.entities, "gate").next().unwrap();
    server.accept_input(id, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 1.0);
    assert_eq!(counter_value(&server, "count"), 2.0);
}

/// `SF_ALLOW_FAST_RETRIGGER` (spawnflag 2) turns the latch off — 784 of the
/// game's relays set it.
#[test]
fn a_fast_retrigger_relay_does_not_latch() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "gate"),
            ("spawnflags", "2"),
            ("OnTrigger", &conn("count", "Add", "1", "0.5", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "pusher"),
            ("OnSpawn", &conn("gate", "Trigger", "", "0", "-1")),
            ("OnSpawn", &conn("gate", "Trigger", "", "0.1", "-1")),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    run(&mut server, 1.0);
    assert_eq!(counter_value(&server, "count"), 2.0);
}

/// A `times-to-fire` of 1 deletes the connection after it fires, so the action
/// list is mutable state rather than a parsed constant. 2,925 shipped
/// connections depend on it.
#[test]
fn a_connection_with_a_fire_limit_deletes_itself() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "gate"),
            ("spawnflags", "2"),
            ("OnTrigger", &conn("count", "Add", "1", "0", "1")),
            ("OnTrigger", &conn("count", "Add", "10", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "1000"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let gate = name::find_by_name(&server.entities, "gate").next().unwrap();

    assert_eq!(find_named(&server, "gate").output_count("OnTrigger"), 2);
    server.accept_input(gate, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(counter_value(&server, "count"), 11.0);
    assert_eq!(
        find_named(&server, "gate").output_count("OnTrigger"),
        1,
        "the limited connection is gone"
    );

    server.accept_input(gate, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(
        counter_value(&server, "count"),
        21.0,
        "only the +10 remains"
    );
}

/// The classname fallback. 2,747 shipped connections fire `SetFogController`
/// at the literal string `env_fog_controller` and reach every one in the map.
#[test]
fn an_unmatched_name_falls_back_to_the_classname() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "gate"),
            ("spawnflags", "2"),
            // No entity is *named* math_counter; two are of that class.
            ("OnTrigger", &conn("math_counter", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "a"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "b"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let gate = name::find_by_name(&server.entities, "gate").next().unwrap();
    server.accept_input(gate, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 0.02);

    assert_eq!(counter_value(&server, "a"), 1.0);
    assert_eq!(counter_value(&server, "b"), 1.0, "both, not just the first");
    assert_eq!(server.io.no_target, 0);
}

/// A name that matches nothing and is no class is counted, not an error.
#[test]
fn an_event_that_reaches_nothing_is_counted() {
    let mut server = Server::new();
    server.level_init("test", &[block(&[("classname", "info_target")])]);
    server.queue.add(Event {
        fire_time: 0.0,
        target: Target::Name(String::from("nobody")),
        input: String::from("Trigger"),
        value: Variant::Void,
        activator: None,
        caller: None,
        output_id: 0,
    });
    run(&mut server, 0.02);
    assert_eq!(server.io.no_target, 1);
    assert_eq!(server.io.accepted, 0);
}

/// > **A per-action parameter override silently discards the caller's extra
/// > delay.** `cbase.cpp:280` against `:289`. Valve's, almost certainly a bug,
/// > and reproduced so that nothing in Chapter 4 goes quietly out of sync.
#[test]
fn a_parameter_override_drops_the_callers_extra_delay() {
    let mut harness = super::test_support::Harness::new();
    let class = classes::lookup("logic_relay").expect("registered");
    let mut entity = super::test_support::entity(class);

    entity
        .core
        .add_connection("OnTrigger", &conn("a", "Add", "", "1.0", "-1"), 1);
    entity
        .core
        .add_connection("OnTrigger", &conn("b", "Add", "7", "1.0", "-1"), 2);

    // `FireOutput( value, activator, caller, fDelay = 0.25 )`.
    {
        let Entity { core, behaviour: _ } = &mut entity;
        let mut cx = class::Context::new(
            harness.clock.time(),
            &mut harness.queue,
            &mut harness.random,
        );
        core.fire_output("OnTrigger", Variant::Void, None, None, 0.25, &mut cx);
    }

    let queued = harness.queued();
    let at = |target: &str| {
        queued
            .iter()
            .find(|(t, ..)| t == target)
            .map(|(.., time)| *time)
            .expect("queued")
    };
    assert_eq!(at("a"), 1.25, "no override: the delays add");
    assert_eq!(at("b"), 1.0, "an override drops the extra delay");
}

/// `PhysicsRunSpecificThink` clears the schedule **before** dispatching, so a
/// think that does not re-arm never runs again — and one that does, does.
#[test]
fn a_think_that_does_not_rearm_runs_once() {
    let map = vec![
        // `logic_relay`'s OnSpawn think re-arms nothing.
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "once"),
            ("OnSpawn", &conn("count", "Add", "1", "0", "-1")),
            ("spawnflags", "2"),
        ]),
        // `logic_timer`'s does, every 0.1 s.
        block(&[
            ("classname", "logic_timer"),
            ("targetname", "tick"),
            ("RefireTime", "0.1"),
            ("OnTimer", &conn("repeats", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "1000"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "repeats"),
            ("max", "1000"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    run(&mut server, 1.0);

    assert_eq!(counter_value(&server, "count"), 1.0, "OnSpawn fires once");
    // Ten 0.1-second intervals in a second; the first fires at 0.1.
    assert_eq!(counter_value(&server, "repeats"), 10.0);
}

/// `Kill` is `CBaseEntity`'s, not the class's — 1,896 shipped connections fire
/// it — and it is deferred like every other removal.
#[test]
fn the_base_kill_input_removes_an_entity_at_the_end_of_the_tick() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "gate"),
            ("spawnflags", "2"),
            ("OnTrigger", &conn("victim", "Kill", "", "0", "-1")),
            // …and something that still reaches the victim in the same tick,
            // because removal is deferred to the end of it.
            ("OnTrigger", &conn("victim", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "victim"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    assert_eq!(server.entities.len(), 2);

    let gate = name::find_by_name(&server.entities, "gate").next().unwrap();
    server.accept_input(gate, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 0.02);

    assert_eq!(server.entities.len(), 1, "the counter is gone");
    assert_eq!(
        server.io.accepted, 3,
        "Trigger, Kill and Add all landed — the Add reached a marked entity"
    );
}

/// The tone mapper is what stage 2 is for, and this is `sp_a1_intro1`'s own
/// chain in miniature: a self-starting relay triggers two exposure relays, one
/// of which is `StartDisabled`, and the survivor sets the ceiling.
#[test]
fn a_map_sets_its_own_exposure_limits() {
    let map = vec![
        block(&[
            ("classname", "env_tonemap_controller"),
            ("targetname", "tonemap_global"),
            ("spawnflags", "1"),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "prestasis"),
            ("StartDisabled", "0"),
            (
                "OnTrigger",
                &conn("tonemap_global", "SetAutoExposureMax", "1.5", "0", "-1"),
            ),
            (
                "OnTrigger",
                &conn("tonemap_global", "SetAutoExposureMin", "1", "0", "-1"),
            ),
            (
                "OnTrigger",
                &conn("tonemap_global", "SetTonemapRate", ".25", "0", "-1"),
            ),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "poststasis"),
            ("StartDisabled", "1"),
            (
                "OnTrigger",
                &conn("tonemap_global", "SetAutoExposureMax", "5", "0", "-1"),
            ),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "lighting_fixup"),
            ("OnSpawn", &conn("prestasis", "Trigger", "", "0", "-1")),
            ("OnSpawn", &conn("poststasis", "Trigger", "", "0", "-1")),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);

    // Before anything runs, the map is asking for nothing.
    assert_eq!(server.tonemap_settings(), TonemapSettings::default());

    run(&mut server, 0.05);
    let settings = server.tonemap_settings();
    assert!(settings.use_custom_auto_exposure_max);
    assert_eq!(
        settings.custom_auto_exposure_max, 1.5,
        "the StartDisabled relay must not have got through with its 5"
    );
    assert!(settings.use_custom_auto_exposure_min);
    assert_eq!(settings.custom_auto_exposure_min, 1.0);
    assert_eq!(settings.rate, 0.25);
}

/// `CTonemapSystem::LevelInitPostEntity`: the first controller is master, and
/// any later flagged one replaces it.
#[test]
fn the_master_tone_mapper_is_the_last_flagged_one() {
    let controller = |name: &str, flags: &str| {
        block(&[
            ("classname", "env_tonemap_controller"),
            ("targetname", name),
            ("spawnflags", flags),
        ])
    };

    // No flags anywhere: the first wins.
    let mut server = Server::new();
    server.level_init("t", &[controller("a", "0"), controller("b", "0")]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "a"
    );

    // The flagged one wins wherever it is.
    server.level_init("t", &[controller("a", "0"), controller("b", "1")]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "b"
    );
    server.level_init("t", &[controller("a", "1"), controller("b", "0")]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "a"
    );
    // Two flagged: the last one.
    server.level_init("t", &[controller("a", "1"), controller("b", "1")]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "b"
    );

    // …and no controller at all is the fallback, not the previous map's.
    server.level_init("t", &[block(&[("classname", "info_target")])]);
    assert_eq!(server.tonemap_settings(), TonemapSettings::default());
}

/// `!self` is the caller and `!activator` is forwarded across a relay chain —
/// the one line in `InputTrigger` that makes a Source map's `!activator` work
/// several hops from the thing that moved.
#[test]
fn the_activator_is_forwarded_across_a_relay_chain() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "first"),
            ("spawnflags", "2"),
            ("OnTrigger", &conn("second", "Trigger", "", "0", "-1")),
        ]),
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "second"),
            ("spawnflags", "2"),
            // `!activator` must still be the entity that started the chain.
            ("OnTrigger", &conn("!activator", "Add", "1", "0", "-1")),
            // `!self` is whoever fired *this* output, i.e. `second`.
            ("OnTrigger", &conn("!self", "FireUser1", "", "0", "-1")),
            ("OnUser1", &conn("witness", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "starter"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "witness"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let starter = name::find_by_name(&server.entities, "starter")
        .next()
        .unwrap();
    let first = name::find_by_name(&server.entities, "first")
        .next()
        .unwrap();

    // The counter named `starter` is the activator of the whole chain.
    server.accept_input(first, "Trigger", Variant::Void, Some(starter), None, 0);
    run(&mut server, 0.05);

    assert_eq!(
        counter_value(&server, "starter"),
        1.0,
        "!activator survived two relays"
    );
    assert_eq!(counter_value(&server, "witness"), 1.0, "!self resolved");
}

/// An input name nothing declares is counted rather than dropped, which is the
/// stage's progress metric.
#[test]
fn an_input_no_class_implements_is_counted() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "100"),
        ])],
    );
    let id = name::find_by_name(&server.entities, "count")
        .next()
        .unwrap();
    assert!(!server.accept_input(id, "SetParent", Variant::Void, None, None, 0));
    assert_eq!(server.io.unhandled.get("math_counter.SetParent"), Some(&1));
}

/// The string a map writes reaches a float handler, because `AcceptInput`
/// converts against the declared type first. Without it every
/// `SetAutoExposureMax 1.5` would arrive as a zero.
#[test]
fn a_string_parameter_converts_to_the_declared_type() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "100"),
        ])],
    );
    let id = name::find_by_name(&server.entities, "count")
        .next()
        .unwrap();

    assert!(server.accept_input(
        id,
        "Add",
        Variant::String(String::from("2.5")),
        None,
        None,
        0
    ));
    assert_eq!(counter_value(&server, "count"), 2.5);

    // A value that cannot convert is refused and counted rather than silently
    // read as zero.
    assert!(!server.accept_input(id, "Add", Variant::Vector(glam::Vec3::ONE), None, None, 0));
    assert_eq!(server.io.bad_conversion, 1);
    assert_eq!(counter_value(&server, "count"), 2.5);
}

/// The tick is fixed and the frame is not: the same simulated time produces
/// the same schedule whatever the frame rate.
#[test]
fn the_schedule_does_not_depend_on_the_frame_rate() {
    let map = vec![
        block(&[
            ("classname", "logic_timer"),
            ("targetname", "tick"),
            ("RefireTime", "0.1"),
            ("OnTimer", &conn("count", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("max", "1000"),
        ]),
    ];

    let at_frame_rate = |fps: f32| {
        let mut server = Server::new();
        server.level_init("test", &map);
        let frame = 1.0 / fps;
        let frames = (1.0 / frame).round() as u32;
        for _ in 0..frames {
            server.frame(frame);
        }
        counter_value(&server, "count")
    };

    assert_eq!(at_frame_rate(300.0), 10.0);
    assert_eq!(at_frame_rate(60.0), 10.0);
    assert_eq!(at_frame_rate(15.0), 10.0);
}

/// `logic_branch` remembers, and `Test` fires without changing anything.
#[test]
fn a_branch_separates_setting_from_testing() {
    let map = vec![
        block(&[
            ("classname", "logic_branch"),
            ("targetname", "flag"),
            ("InitialValue", "0"),
            ("OnTrue", &conn("yes", "Add", "1", "0", "-1")),
            ("OnFalse", &conn("no", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "yes"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "no"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let flag = name::find_by_name(&server.entities, "flag").next().unwrap();

    // `SetValue` changes the value and fires nothing — 1,175 of the game's
    // 1,601 connections into a branch are this.
    server.accept_input(flag, "SetValue", Variant::Bool(true), None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(
        (counter_value(&server, "yes"), counter_value(&server, "no")),
        (0.0, 0.0)
    );

    server.accept_input(flag, "Test", Variant::Void, None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(
        (counter_value(&server, "yes"), counter_value(&server, "no")),
        (1.0, 0.0)
    );

    server.accept_input(flag, "ToggleTest", Variant::Void, None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(
        (counter_value(&server, "yes"), counter_value(&server, "no")),
        (1.0, 1.0)
    );
}

/// `logic_case`'s `PickRandom` chooses among the cases whose **output** has
/// connections, not the ones whose *value* is set — which is what lets Portal 2
/// use it as a random picker with no case values at all.
#[test]
fn pick_random_chooses_among_connected_outputs() {
    let map = vec![
        block(&[
            ("classname", "logic_case"),
            ("targetname", "pick"),
            // Sixteen case values, but only three outputs connected.
            ("Case01", "a"),
            ("Case02", "b"),
            ("OnCase03", &conn("three", "Add", "1", "0", "-1")),
            ("OnCase05", &conn("five", "Add", "1", "0", "-1")),
            ("OnCase07", &conn("seven", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "three"),
            ("max", "1000"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "five"),
            ("max", "1000"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "seven"),
            ("max", "1000"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let pick = name::find_by_name(&server.entities, "pick").next().unwrap();

    for _ in 0..90 {
        server.accept_input(pick, "PickRandom", Variant::Void, None, None, 0);
        run(&mut server, 0.02);
    }
    let counts = [
        counter_value(&server, "three"),
        counter_value(&server, "five"),
        counter_value(&server, "seven"),
    ];
    assert_eq!(counts.iter().sum::<f32>(), 90.0);
    for count in counts {
        assert!(
            count > 0.0,
            "every connected case should come up: {counts:?}"
        );
    }
}

/// `InValue` matches against the value's *printed* form, so a float parameter
/// matches a case spelled as a decimal.
#[test]
fn in_value_matches_a_case_by_its_string_form() {
    let map = vec![
        block(&[
            ("classname", "logic_case"),
            ("targetname", "pick"),
            ("Case01", "red"),
            ("Case02", "1.5"),
            ("OnCase01", &conn("red", "Add", "1", "0", "-1")),
            ("OnCase02", &conn("num", "Add", "1", "0", "-1")),
            ("OnDefault", &conn("other", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "red"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "num"),
            ("max", "100"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "other"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let pick = name::find_by_name(&server.entities, "pick").next().unwrap();
    let fire = |server: &mut Server, value: Variant| {
        server.accept_input(pick, "InValue", value, None, None, 0);
        run(server, 0.02);
    };

    fire(&mut server, Variant::String(String::from("red")));
    fire(&mut server, Variant::Float(1.5));
    fire(&mut server, Variant::String(String::from("nothing")));

    assert_eq!(counter_value(&server, "red"), 1.0);
    assert_eq!(counter_value(&server, "num"), 1.0, "1.5 printed as \"1.5\"");
    assert_eq!(counter_value(&server, "other"), 1.0);
}

/// `math_counter`'s edge outputs fire on the edge, not on every input once the
/// ceiling is reached.
#[test]
fn a_counter_fires_its_ceiling_once() {
    let map = vec![
        block(&[
            ("classname", "math_counter"),
            ("targetname", "count"),
            ("min", "0"),
            ("max", "3"),
            ("startvalue", "0"),
            ("OnHitMax", &conn("hits", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "hits"),
            ("max", "100"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map);
    let count = name::find_by_name(&server.entities, "count")
        .next()
        .unwrap();
    for _ in 0..6 {
        server.accept_input(count, "Add", Variant::Float(1.0), None, None, 0);
        run(&mut server, 0.02);
    }

    assert_eq!(counter_value(&server, "count"), 3.0, "clamped to max");
    assert_eq!(counter_value(&server, "hits"), 1.0, "and OnHitMax once");
}

// ---------------------------------------------------------------------------
// the depot
// ---------------------------------------------------------------------------

/// The 28 key names nothing consumes, across all 106 shipped maps, with how
/// often each appears.
///
/// This is the parse-side status in one table, and it is worth reading rather
/// than skipping:
///
/// - **17 are the map compiler's.** `_light`, `_lightHDR`, `_quadratic_attn`
///   and the rest of the falloff family are read by `vbsp`/`vrad` at compile
///   time (`utils/vbsp/map.cpp`) and have **no run-time consumer in Valve's
///   engine either** — the shipped server drops them exactly as this one does.
///   `detailvbsp` is `vbsp`'s, `paintinmap` is read by `engine/cmodel.cpp` and
///   `mapversion` appears nowhere in the tree at all.
/// - **6 are mapper mistakes shipped in the game**: a `logic_relay` with a key
///   called `//OnTrigger`, seventeen with `_OnTrigger`, two with
///   `OnUnPressed`, a `light_spot` with `AddonPoints`/`NpcPoints` from a
///   different entity's FGD, a `logic_auto` in `mp_coop_paint_longjump_intro`
///   with an `OnTrigger` it has no output for, and one in `sp_a4_finale2` with
///   `_OnMapSpawn`. **The last two are new at stage 2 and are progress**: they
///   only became visible once `logic_auto` was implemented, because until then
///   the whole entity was an unknown classname and its keys were never
///   counted. Both are dead in Valve's engine too.
/// - **`OnProxyRelay` (135)** is the unnumbered output the FGD shows a mapper;
///   Hammer's instance compiler is supposed to rewrite it into a numbered one
///   and these were missed. No version of the server has ever handled it.
/// - The rest are genuinely not implemented yet, and all of them are small:
///   `vscripts` (`portdocs/SERVER.md` §9), `SunSpreadAngle`, `ambient`.
///
/// A new name appearing here is a regression; a name leaving it is progress.
/// Either way this table changes and the change should be deliberate.
const EXPECTED_UNHANDLED: &[(&str, usize)] = &[
    ("//ontrigger", 1),
    ("_ambienthdr", 25),
    ("_ambientscalehdr", 25),
    ("_cone", 2470),
    ("_constant_attn", 4599),
    ("_distance", 4059),
    ("_exponent", 2470),
    ("_fifty_percent_distance", 4292),
    ("_hardfalloff", 3900),
    ("_inner_cone", 2470),
    ("_light", 7125),
    ("_lighthdr", 7150),
    ("_lightscalehdr", 7150),
    ("_linear_attn", 4096),
    ("_onmapspawn", 1),
    ("_ontrigger", 17),
    ("_quadratic_attn", 7121),
    ("_zero_percent_distance", 4292),
    ("addonpoints", 1),
    ("ambient", 2),
    ("detailvbsp", 106),
    ("mapversion", 106),
    ("npcpoints", 1),
    ("onproxyrelay", 135),
    ("ontrigger", 5),
    ("onunpressed", 2),
    ("paintinmap", 25),
    ("sunspreadangle", 27),
    ("vscripts", 1),
];

/// Every shipped map's entity lump, spawned and then **run** for two seconds
/// of server time.
///
/// Ignored by default and gated on `KISAK_GAME_DIR`, like the `world/`,
/// `studio/` and `trace/` depot tests. Needs no GPU: the `.bsp` is read for
/// its entity lump and nothing else.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_maps -- --ignored --nocapture
/// ```
///
/// Two seconds is chosen rather than guessed: `logic_auto`'s bootstrap is at
/// 0.2 s and `logic_relay`'s `OnSpawn` at 0.01 s, so two seconds covers every
/// map's start-up plus a second of whatever it set going. The numbers below
/// are exact, because the files are.
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_map_spawns_its_entities() {
    use crate::engine::world::bsp::Bsp;
    use crate::filesystem::Vfs;
    use std::collections::BTreeMap;

    /// How long to run each map for, in seconds of server time.
    const RUN_SECONDS: f32 = 2.0;

    let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
        panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
    };
    let dir = std::path::PathBuf::from(dir);
    let base = dir.parent().unwrap_or(&dir).to_path_buf();
    let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

    let mut names: Vec<String> = vfs
        .list("maps")
        .expect("maps/")
        .into_iter()
        .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
        .map(|e| e.name.trim_end_matches(".bsp").to_owned())
        .collect();
    names.sort();
    assert!(names.len() > 50, "only {} maps found", names.len());

    let mut server = Server::new();
    let mut total = LevelStats::default();
    let mut io = IoStats::default();
    let mut per_class: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut named_lights = 0;
    let mut maps_with_a_master = 0;
    let mut custom_max = 0;
    let mut peak_thinks = 0;

    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let stats = server.level_init(name, &bsp.entities());

        // Per map: nothing may be lost. Every block is matched or unknown, and
        // every matched entity is alive or removed itself.
        assert_eq!(
            stats.matched + stats.unknown.values().sum::<usize>(),
            stats.blocks,
            "{name}: blocks unaccounted for"
        );
        assert_eq!(
            stats.spawned + stats.removed_on_spawn,
            stats.matched,
            "{name}: entities unaccounted for"
        );
        assert_eq!(stats.spawned, server.entities.len());
        // Exactly one world, always, and it is the lump's first block.
        assert_eq!(
            server
                .entities
                .iter()
                .filter(|(_, e)| e.classname() == "worldspawn")
                .count(),
            1,
            "{name}: not exactly one worldspawn"
        );

        for (_, entity) in server.entities.iter() {
            *per_class.entry(entity.classname()).or_default() += 1;
            if entity.classname().starts_with("light") {
                assert!(
                    entity.name.is_some(),
                    "{name}: an unnamed light survived Spawn"
                );
                named_lights += 1;
            }
        }

        total.blocks += stats.blocks;
        total.matched += stats.matched;
        total.spawned += stats.spawned;
        total.removed_on_spawn += stats.removed_on_spawn;
        total.outputs += stats.outputs;
        total.parented += stats.parented;
        total.parents_missing += stats.parents_missing;
        for (key, count) in &stats.unknown {
            *total.unknown.entry(key.clone()).or_default() += count;
        }
        for (key, count) in &stats.unhandled {
            *total.unhandled.entry(key.clone()).or_default() += count;
        }

        // …and now run it. Nothing here may panic, and the think list must not
        // grow without bound.
        let interval = server.time().interval;
        let ticks = (RUN_SECONDS / interval).round() as u32;
        for _ in 0..ticks {
            server.frame(interval);
            peak_thinks = peak_thinks.max(server.thinks.len());
        }

        if server.master_tonemap.is_some() {
            maps_with_a_master += 1;
        }
        if server.tonemap_settings().use_custom_auto_exposure_max {
            custom_max += 1;
        }

        io.dispatched += server.io.dispatched;
        io.accepted += server.io.accepted;
        io.no_target += server.io.no_target;
        io.bad_conversion += server.io.bad_conversion;
        io.thinks += server.io.thinks;
        for (key, count) in &server.io.unhandled {
            *io.unhandled.entry(key.clone()).or_default() += count;
        }
    }

    println!("\n{} maps: {}", names.len(), total.summary());
    println!("  live entities by class:");
    for (classname, count) in &per_class {
        println!("    {count:>7}  {classname}");
    }
    println!(
        "  {} unimplemented classnames, {} occurrences",
        total.unknown.len(),
        total.unknown.values().sum::<usize>()
    );
    let mut unknown: Vec<_> = total.unknown.iter().collect();
    unknown.sort_by(|a, b| b.1.cmp(a.1));
    for (classname, count) in unknown.iter().take(10) {
        println!("    {count:>7}  {classname}");
    }
    println!("  keys nothing consumed:");
    for (key, count) in &total.unhandled {
        println!("    {count:>7}  {key}");
    }
    println!(
        "\n  after {RUN_SECONDS}s of server time per map:\n    \
         {} events dispatched, {} inputs accepted, {} thinks run\n    \
         {} events found no target, {} values would not convert\n    \
         {} maps have a master tone mapper, {} of them set a custom ceiling\n    \
         at most {peak_thinks} entities thinking at once",
        io.dispatched,
        io.accepted,
        io.thinks,
        io.no_target,
        io.bad_conversion,
        maps_with_a_master,
        custom_max
    );
    println!("  inputs nothing handled:");
    let mut unhandled_inputs: Vec<_> = io.unhandled.iter().collect();
    unhandled_inputs.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (input, count) in unhandled_inputs.iter().take(40) {
        println!("    {count:>7}  {input}");
    }
    println!(
        "    ({} distinct, {} occurrences)",
        io.unhandled.len(),
        io.unhandled.values().sum::<usize>()
    );

    assert_eq!(names.len(), 106, "Portal 2 ships 106 maps");
    assert_eq!(total.blocks, 60_925);
    assert_eq!(total.spawned + total.removed_on_spawn, total.matched);
    assert_eq!(total.removed_on_spawn, 6_937, "unnamed lights");

    // The parse side. Stage 1 matched 17,069 blocks and spawned 10,132; the
    // six classes stage 2 added are the difference.
    assert_eq!(total.matched, 19_229);
    assert_eq!(total.spawned, 12_292);
    assert_eq!(total.outputs, 46_489);
    assert_eq!(total.unknown.len(), 185);
    assert_eq!(total.unknown.values().sum::<usize>(), 41_696);
    assert_eq!(
        named_lights, 213,
        "lights that survive because they are named"
    );
    assert_eq!(
        total
            .unhandled
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        EXPECTED_UNHANDLED,
        "the set of keys nothing consumes has changed"
    );

    // The census `portdocs/SERVER.md` §1.2 is built on, re-derived from the
    // live list rather than from the lump. These are the classes nothing
    // deletes at run time, so the counts are the ones the maps place.
    assert_eq!(per_class.get("func_instance_io_proxy"), Some(&1_184));
    assert_eq!(per_class.get("info_target"), Some(&431));
    assert_eq!(per_class.get("info_player_start"), Some(&116));
    assert_eq!(per_class.get("worldspawn"), Some(&106));
    assert_eq!(per_class.get("env_tonemap_controller"), Some(&110));
    assert_eq!(per_class.get("logic_branch"), Some(&601));
    assert_eq!(per_class.get("logic_case"), Some(&84));
    assert_eq!(per_class.get("logic_timer"), Some(&151));
    assert_eq!(per_class.get("math_counter"), Some(&102));

    // 105 of the 106 maps place a tone mapper; `sp_a5_credits` is the one that
    // does not.
    assert_eq!(maps_with_a_master, 105);
    // …and 100 of those 105 have set a custom ceiling within two seconds. The
    // five that have not are maps whose controller is driven by something
    // later than the bootstrap — a trigger the player has to walk into.
    assert_eq!(custom_max, 100);

    // The run side. These are what two seconds of every shipped map does.
    assert_eq!(io.dispatched, 5_763);
    assert_eq!(io.accepted, 2_070);
    assert_eq!(io.thinks, 1_197);
    // Most events reach nothing because most *targets* are entities of classes
    // this port has not got — `prop_dynamic` alone is 8,072 of them. Expect
    // this number to fall as classes land.
    assert_eq!(io.no_target, 3_700);

    // Nothing may fail to convert: every shipped connection's parameter is
    // compatible with the input it is aimed at.
    assert_eq!(io.bad_conversion, 0, "a shipped map has a bad I/O link");

    // The whole set of inputs that reach an implemented class and are refused.
    // Three of the seven are the player procedurals, which are stage 5's; the
    // rest are `CBaseEntity`'s parenting family, which is stage 3's, and one
    // `RunScriptCode` (`portdocs/SERVER.md` §9).
    let unhandled: Vec<(&str, usize)> =
        io.unhandled.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        unhandled,
        vec![
            ("!player (needs a player)", 97),
            ("!player_blue (needs a player)", 37),
            ("!player_orange (needs a player)", 37),
            ("info_target.SetParent", 1),
            ("info_target.SetParentAttachment", 12),
            ("info_target.SetParentAttachmentMaintainOffset", 1),
            ("logic_relay.RunScriptCode", 1),
        ],
        "the set of inputs nothing handles has changed"
    );

    // `ThinkList` is a flat `Vec` with a linear scan, which is only the right
    // shape while this number is small. It is the measurement `think.rs` cites.
    assert_eq!(peak_thinks, 43);

    // The one map this port looks at most, and the headline of the whole
    // stage: `sp_a1_intro1` asks for a ceiling of 1.5 against the cvar default
    // of 2, through `logic_relay @rl_lighting_fixup`'s `OnSpawn` →
    // `@rl_prestasis_exposure_reload` → the controller. Its sibling
    // `@rl_poststasis_exposure_reload` is triggered by the same `OnSpawn` and
    // asks for 5; it is `StartDisabled 1`, so honouring that key is what
    // decides which number the map gets.
    let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("the intro map parses");
    server.level_init("sp_a1_intro1", &bsp.entities());
    let interval = server.time().interval;
    for _ in 0..(RUN_SECONDS / interval).round() as u32 {
        server.frame(interval);
    }
    let settings = server.tonemap_settings();
    assert!(settings.use_custom_auto_exposure_max);
    assert_eq!(settings.custom_auto_exposure_max, 1.5);
    assert_eq!(settings.custom_auto_exposure_min, 1.0);
    assert_eq!(settings.rate, 0.25);
}
