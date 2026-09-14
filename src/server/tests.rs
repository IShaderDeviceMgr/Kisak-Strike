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
    let stats = server.level_init("test", &sample(), &[]);

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
    server.level_init("test", &sample(), &[]);

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
        &[],
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
        &[],
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
        &[],
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
        &[],
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
    server.level_init("first", &sample(), &[]);
    assert_eq!(server.entities.len(), 5);

    let stats = server.level_init("second", &[block(&[("classname", "info_target")])], &[]);
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
        &[],
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
        server.frame(interval, &mut NoTouchQuery);
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
        &[],
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
    server.level_init("test", &relays, &[]);

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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &[block(&[("classname", "info_target")])], &[]);
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
        let mut cx = harness.context();
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);

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
    server.level_init("t", &[controller("a", "0"), controller("b", "0")], &[]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "a"
    );

    // The flagged one wins wherever it is.
    server.level_init("t", &[controller("a", "0"), controller("b", "1")], &[]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "b"
    );
    server.level_init("t", &[controller("a", "1"), controller("b", "0")], &[]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "a"
    );
    // Two flagged: the last one.
    server.level_init("t", &[controller("a", "1"), controller("b", "1")], &[]);
    assert_eq!(
        server
            .entities
            .get(server.master_tonemap.unwrap())
            .unwrap()
            .debug_name(),
        "b"
    );

    // …and no controller at all is the fallback, not the previous map's.
    server.level_init("t", &[block(&[("classname", "info_target")])], &[]);
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
    server.level_init("test", &map, &[]);
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
        &[],
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
        &[],
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
        server.level_init("test", &map, &[]);
        let frame = 1.0 / fps;
        let frames = (1.0 / frame).round() as u32;
        for _ in 0..frames {
            server.frame(frame, &mut NoTouchQuery);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
    server.level_init("test", &map, &[]);
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
// stage 3: brush entities move
// ---------------------------------------------------------------------------

/// A brush model's bounding box, for the movers that measure themselves.
fn model(mins: [f32; 3], maxs: [f32; 3]) -> bsp::Model {
    bsp::Model {
        mins,
        maxs,
        origin: [0.0; 3],
        head_node: 0,
        first_face: 0,
        num_faces: 0,
    }
}

/// Model 0 is the world; model 1 is a door-shaped slab **66x10x66**, so that
/// after `CBaseDoor::Spawn`'s `vecOBB -= Vector(2,2,2)` the travel is a round
/// 64 along X or Z and 8 along Y.
fn door_models() -> Vec<bsp::Model> {
    vec![
        model([-512.0, -512.0, -512.0], [512.0, 512.0, 512.0]),
        model([-33.0, -5.0, -33.0], [33.0, 5.0, 33.0]),
    ]
}

fn placement(server: &Server, name: &str) -> (glam::Vec3, glam::Vec3) {
    let entity = find_named(server, name);
    (entity.origin, entity.angles)
}

/// Positions are compared loosely, and the reason is worth knowing:
/// `movedir "-90 0 0"` is `AngleVectors` of a 90-degree pitch, whose cosine is
/// 4.4e-8 rather than 0 — so a door that travels 64 units "straight up" also
/// travels 2.8 millionths of a unit sideways. Valve's arithmetic has exactly
/// the same residue; asserting an exact zero would be asserting something the
/// shipped game does not do either.
#[track_caller]
fn close(actual: glam::Vec3, expected: glam::Vec3) {
    assert!(
        (actual - expected).length() < 1e-3,
        "{actual} is not {expected}"
    );
}

/// The headline of the stage: a door told to open travels, arrives on time,
/// lands exactly on its destination and says so.
#[test]
fn a_door_opens_and_fires_its_arrival() {
    let map = vec![
        block(&[
            ("classname", "func_door"),
            ("targetname", "door"),
            ("model", "*1"),
            ("origin", "0 0 0"),
            // Straight up, 64 units of model along Z, no lip: a 64-unit
            // travel at 64 units a second is exactly one second.
            ("movedir", "-90 0 0"),
            ("lip", "0"),
            ("speed", "64"),
            ("wait", "-1"),
            ("OnFullyOpen", &conn("opened", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "opened"),
            ("max", "10"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    close(placement(&server, "door").0, glam::Vec3::ZERO);

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    // Half a second in, it is half way and has not arrived.
    run(&mut server, 0.5);
    let origin = placement(&server, "door").0;
    assert!(
        (origin.z - 32.0).abs() < 0.5,
        "half a second is half the travel, not {origin}"
    );
    assert_eq!(counter_value(&server, "opened"), 0.0);

    // A second in, it is there, exactly, and `OnFullyOpen` has gone out.
    run(&mut server, 0.6);
    close(
        placement(&server, "door").0,
        glam::Vec3::new(0.0, 0.0, 64.0),
    );
    assert_eq!(counter_value(&server, "opened"), 1.0);

    // `wait -1` means it stays there and stops simulating.
    run(&mut server, 2.0);
    close(
        placement(&server, "door").0,
        glam::Vec3::new(0.0, 0.0, 64.0),
    );
    assert_eq!(server.thinks.len(), 0, "and it leaves the simulation list");
}

/// A `wait` that is not `-1` closes the door again — on the **arrival alarm**,
/// with the door standing still, which is the mechanism that makes
/// `set_move_done_time` a separate timer from the think schedule.
#[test]
fn a_door_with_a_wait_closes_itself_on_the_same_alarm() {
    let map = vec![
        block(&[
            ("classname", "func_door"),
            ("targetname", "door"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            ("lip", "0"),
            ("speed", "64"),
            ("wait", "0.5"),
            ("OnFullyClosed", &conn("closed", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "closed"),
            ("max", "10"),
        ]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    // 1 s out, 0.5 s waiting, 1 s back.
    run(&mut server, 1.2);
    assert!(placement(&server, "door").0.z > 63.0, "open");
    assert_eq!(counter_value(&server, "closed"), 0.0);

    run(&mut server, 1.6);
    close(placement(&server, "door").0, glam::Vec3::ZERO);
    assert_eq!(counter_value(&server, "closed"), 1.0);
}

/// `func_door_rotating` turns instead of sliding, `distance` is the angle, and
/// `speed` is degrees a second along the axis the spawnflags chose.
#[test]
fn a_rotating_door_turns_about_the_axis_its_spawnflags_name() {
    let yaw = vec![block(&[
        ("classname", "func_door_rotating"),
        ("targetname", "door"),
        ("model", "*1"),
        ("distance", "90"),
        ("speed", "90"),
        ("wait", "-1"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &yaw, &door_models());
    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    assert_eq!(
        placement(&server, "door").1,
        glam::Vec3::new(0.0, 90.0, 0.0),
        "yaw by default"
    );

    // `SF_DOOR_ROTATE_PITCH` (128) plus `SF_DOOR_ROTATE_BACKWARDS` (2): the
    // pitch axis, negated. 143 of the game's rotating doors take the first.
    let pitch = vec![block(&[
        ("classname", "func_door_rotating"),
        ("targetname", "door"),
        ("model", "*1"),
        ("spawnflags", "130"),
        ("distance", "90"),
        ("speed", "90"),
        ("wait", "-1"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &pitch, &door_models());
    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    close(
        placement(&server, "door").1,
        glam::Vec3::new(-90.0, 0.0, 0.0),
    );
}

/// `spawnpos 1` puts the door at its open position before anything runs — the
/// 40 doors in the game that are already open when the level starts.
#[test]
fn a_door_can_spawn_open() {
    let map = vec![
        block(&[
            ("classname", "func_door"),
            ("targetname", "slider"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            ("lip", "0"),
            ("spawnpos", "1"),
        ]),
        block(&[
            ("classname", "func_door_rotating"),
            ("targetname", "turner"),
            ("model", "*1"),
            ("distance", "90"),
            ("spawnpos", "1"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());

    close(
        placement(&server, "slider").0,
        glam::Vec3::new(0.0, 0.0, 64.0),
    );
    close(
        placement(&server, "turner").1,
        glam::Vec3::new(0.0, 90.0, 0.0),
    );
    // And neither is moving.
    assert_eq!(server.thinks.len(), 0);
}

/// A locked door refuses `Open` and takes `Close`, which is Valve's asymmetry:
/// `InputClose` does not test `m_bLocked` and `InputOpen` does.
#[test]
fn a_locked_door_refuses_to_open_and_still_closes() {
    let map = vec![block(&[
        ("classname", "func_door"),
        ("targetname", "door"),
        ("model", "*1"),
        ("movedir", "-90 0 0"),
        ("lip", "0"),
        ("speed", "64"),
        ("wait", "-1"),
        ("spawnpos", "1"),
        // `SF_DOOR_LOCKED`.
        ("spawnflags", "2048"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let door = find_named(&server, "door").id();

    server.accept_input(door, "Close", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    close(placement(&server, "door").0, glam::Vec3::ZERO);

    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    close(placement(&server, "door").0, glam::Vec3::ZERO);

    server.accept_input(door, "Unlock", Variant::Void, None, None, 0);
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    close(
        placement(&server, "door").0,
        glam::Vec3::new(0.0, 0.0, 64.0),
    );
}

/// A `func_movelinear` with `startposition 1` spawns at the *open* end, so its
/// closed end is computed backwards from where the mapper drew it.
#[test]
fn a_movelinear_measures_its_ends_from_where_it_was_drawn() {
    let map = vec![
        block(&[
            ("classname", "func_movelinear"),
            ("targetname", "piston"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            ("movedistance", "100"),
            ("startposition", "1"),
            ("speed", "100"),
            ("origin", "0 0 200"),
            ("OnFullyClosed", &conn("shut", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "shut"),
            ("max", "10"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let piston = find_named(&server, "piston").id();

    // Drawn at 200 and one whole move-distance along, so closed is at 100.
    close(
        placement(&server, "piston").0,
        glam::Vec3::new(0.0, 0.0, 200.0),
    );
    server.accept_input(piston, "Close", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    close(
        placement(&server, "piston").0,
        glam::Vec3::new(0.0, 0.0, 100.0),
    );
    assert_eq!(counter_value(&server, "shut"), 1.0);

    // `SetPosition` takes a fraction of the way along.
    server.accept_input(piston, "SetPosition", Variant::Float(0.25), None, None, 0);
    run(&mut server, 1.1);
    close(
        placement(&server, "piston").0,
        glam::Vec3::new(0.0, 0.0, 125.0),
    );
}

/// A button goes in, fires `OnIn`, waits on the **think** schedule, comes back
/// out and fires `OnOut` — the full cycle, and the one class here that uses a
/// think rather than the alarm for its wait.
#[test]
fn a_button_presses_in_and_returns_by_itself() {
    let map = vec![
        block(&[
            ("classname", "func_button"),
            ("targetname", "button"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            ("lip", "0"),
            ("speed", "60"),
            ("wait", "0.5"),
            ("OnPressed", &conn("pressed", "Add", "1", "0", "-1")),
            ("OnIn", &conn("inside", "Add", "1", "0", "-1")),
            ("OnOut", &conn("outside", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "pressed"),
            ("max", "10"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "inside"),
            ("max", "10"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "outside"),
            ("max", "10"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let button = find_named(&server, "button").id();

    server.accept_input(button, "Press", Variant::Void, None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(counter_value(&server, "pressed"), 1.0, "at once");
    assert_eq!(counter_value(&server, "inside"), 0.0, "not yet");

    run(&mut server, 1.1);
    assert_eq!(counter_value(&server, "inside"), 1.0, "arrived");
    assert_eq!(counter_value(&server, "outside"), 0.0);
    // > **A button with no `lip` gets a lip of 4**, not 0, so the travel is
    // > the model's 64 less 4. `CBaseButton::Spawn` is the only one of the
    // > three that substitutes a default here.
    close(
        placement(&server, "button").0,
        glam::Vec3::new(0.0, 0.0, 60.0),
    );

    // 0.5 s of wait plus 1 s back.
    run(&mut server, 1.6);
    assert_eq!(counter_value(&server, "outside"), 1.0);
    close(placement(&server, "button").0, glam::Vec3::ZERO);
}

/// 53 of the game's 64 buttons are `SF_BUTTON_DONTMOVE`, which collapses the
/// travel to nothing — and the cycle still has to run, because
/// `LinearMove` calls `MoveDone` itself when it is already there.
#[test]
fn a_button_that_does_not_move_still_completes_its_cycle() {
    let map = vec![
        block(&[
            ("classname", "func_button"),
            ("targetname", "button"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            // `SF_BUTTON_DONTMOVE`.
            ("spawnflags", "1"),
            ("speed", "5"),
            ("wait", "0.1"),
            ("OnIn", &conn("inside", "Add", "1", "0", "-1")),
            ("OnOut", &conn("outside", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "inside"),
            ("max", "10"),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "outside"),
            ("max", "10"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let button = find_named(&server, "button").id();

    server.accept_input(button, "Press", Variant::Void, None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(counter_value(&server, "inside"), 1.0, "in with no travel");
    close(placement(&server, "button").0, glam::Vec3::ZERO);

    run(&mut server, 0.3);
    assert_eq!(counter_value(&server, "outside"), 1.0, "and back out");
}

/// The empty-input `Use` reaches `m_pfnUse`, which two classes here set. A
/// button's ignores the use type, which is what saves it from the connection
/// serial number `InputUse` passes as one.
#[test]
fn a_use_with_no_input_name_presses_a_button() {
    let map = vec![
        block(&[
            ("classname", "logic_relay"),
            ("targetname", "start"),
            // An empty input field, which `EventAction::parse` turns into
            // `Use` — 22 shipped connections do this.
            ("OnTrigger", &conn("button", "", "", "0", "-1")),
        ]),
        block(&[
            ("classname", "func_button"),
            ("targetname", "button"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("wait", "-1"),
            ("OnPressed", &conn("pressed", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "pressed"),
            ("max", "10"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let start = find_named(&server, "start").id();
    server.accept_input(start, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 0.1);
    assert_eq!(counter_value(&server, "pressed"), 1.0);
}

/// `func_rotating` spins up to `maxspeed` and reports it, and `Stop` brings it
/// back to a standstill. With no `SF_BRUSH_ACCDCC` the speed changes instantly.
#[test]
fn a_rotator_starts_stops_and_reports_its_speed() {
    let map = vec![
        block(&[
            ("classname", "func_rotating"),
            ("targetname", "fan"),
            ("model", "*1"),
            ("maxspeed", "180"),
            ("fanfriction", "100"),
            (
                "OnGetSpeed",
                &conn("speed", "SetValueNoFire", "", "0", "-1"),
            ),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "speed"),
            ("max", "1000"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let fan = find_named(&server, "fan").id();

    server.accept_input(fan, "Start", Variant::Void, None, None, 0);
    run(&mut server, 1.0);
    let angles = placement(&server, "fan").1;
    assert!(
        (angles.y - 180.0).abs() < 4.0,
        "a second at 180 deg/s is half a turn, not {angles}"
    );

    // `GetSpeed` reports the magnitude through an output.
    server.accept_input(fan, "GetSpeed", Variant::Void, None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(counter_value(&server, "speed"), 180.0);

    server.accept_input(fan, "Stop", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    let stopped = placement(&server, "fan").1;
    run(&mut server, 1.0);
    assert_eq!(placement(&server, "fan").1, stopped, "and it stays put");
}

/// `SF_BRUSH_ROTATE_START_ON` turns itself on 0.2 s in, through
/// `SUB_CallUseToggle` — a *think* that calls `Use`, which is the only place
/// in the port where those two meet. 18 of the game's 27 rotators.
#[test]
fn a_rotator_that_starts_on_needs_no_input() {
    let map = vec![block(&[
        ("classname", "func_rotating"),
        ("targetname", "fan"),
        ("model", "*1"),
        ("maxspeed", "90"),
        ("fanfriction", "100"),
        // `SF_BRUSH_ROTATE_START_ON` | `SF_BRUSH_ROTATE_Z_AXIS`.
        ("spawnflags", "5"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());

    run(&mut server, 0.15);
    assert_eq!(placement(&server, "fan").1, glam::Vec3::ZERO, "not yet");

    run(&mut server, 1.0);
    let angles = placement(&server, "fan").1;
    assert!(angles.z > 45.0, "spinning about Z by now, not {angles}");
    assert_eq!(angles.x, 0.0);
    assert_eq!(angles.y, 0.0);
}

/// `func_brush` is `StartDisabled` and `Enable`/`Disable`, which is where
/// `portdocs/SERVER.md` §7.4's promise lands: the effect bit and the solidity
/// flag are what `world/` and the trace read.
#[test]
fn a_func_brush_switches_itself_off_and_on() {
    use crate::server::movement::{EF_NODRAW, FSOLID_NOT_SOLID};

    let map = vec![
        block(&[
            ("classname", "func_brush"),
            ("targetname", "panel"),
            ("model", "*1"),
            ("StartDisabled", "1"),
        ]),
        block(&[
            ("classname", "func_brush"),
            ("targetname", "wall"),
            ("model", "*1"),
            ("Solidity", "2"),
            ("StartDisabled", "1"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());

    let panel = find_named(&server, "panel");
    assert_eq!(panel.effects & EF_NODRAW, EF_NODRAW, "invisible");
    assert_eq!(panel.solid_flags & FSOLID_NOT_SOLID, FSOLID_NOT_SOLID);

    // `Solidity 2` is `BRUSHSOLID_ALWAYS`, so this one is invisible and still
    // solid — the asymmetry `CFuncBrush::TurnOff` deliberately has.
    let wall = find_named(&server, "wall");
    assert_eq!(wall.effects & EF_NODRAW, EF_NODRAW);
    assert_eq!(wall.solid_flags & FSOLID_NOT_SOLID, 0, "always solid");

    let panel_id = panel.id();
    server.accept_input(panel_id, "Enable", Variant::Void, None, None, 0);
    let panel = find_named(&server, "panel");
    assert_eq!(panel.effects & EF_NODRAW, 0, "visible again");
    assert_eq!(panel.solid_flags & FSOLID_NOT_SOLID, 0);

    server.accept_input(panel_id, "Toggle", Variant::Void, None, None, 0);
    assert_eq!(find_named(&server, "panel").effects & EF_NODRAW, EF_NODRAW);
}

/// The seam `world/` and `trace/` read: a brush entity's placement is looked
/// up by its `"*N"` model index, and that index is unique across every shipped
/// map.
#[test]
fn a_brush_entity_is_found_by_its_model_index() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "func_door"),
            ("targetname", "door"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            ("lip", "0"),
            ("speed", "64"),
            ("wait", "-1"),
        ]),
        // A classname the port has no implementation for: no placement, so
        // whoever asks leaves it where the lump put it — and, since stage 4,
        // leaves it out of the player's clip chain too. `func_portal_bumper`
        // is the ninth commonest classname in the game and is exactly the
        // reason that rule exists: 2,383 of them, none solid to a player.
        block(&[("classname", "func_portal_bumper"), ("model", "*2")]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());

    assert_eq!(server.brush_entity_count(), 1);
    assert!(
        server.brush_entity(2).is_none(),
        "func_portal_bumper has no class"
    );
    assert!(server.brush_entity(0).is_none(), "model 0 is the world");
    close(
        server.brush_entity(1).expect("the door").origin,
        glam::Vec3::ZERO,
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 1.1);
    close(
        server.brush_entity(1).expect("the door").origin,
        glam::Vec3::new(0.0, 0.0, 64.0),
    );
}

/// A door with nowhere to go arrives **inside** `LinearMove`, so its
/// `OnFullyOpen` reaches the queue before its `OnOpen` — the reverse of the
/// order the two lines appear in `DoorGoUp`.
///
/// The counter starts at 0; `OnFullyOpen` adds one and `OnOpen` doubles, so
/// arriving first reads 2 and arriving second reads 1.
#[test]
fn a_zero_length_open_arrives_before_it_announces_itself() {
    let map = vec![
        block(&[
            ("classname", "func_door"),
            ("targetname", "door"),
            ("model", "*1"),
            ("movedir", "-90 0 0"),
            // The model is 64 along Z after Valve's two units, so a lip of 64
            // leaves it nowhere to travel.
            ("lip", "64"),
            ("speed", "100"),
            ("wait", "-1"),
            ("OnFullyOpen", &conn("order", "Add", "1", "0", "-1")),
            ("OnOpen", &conn("order", "Multiply", "2", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "order"),
            ("max", "100"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 0.1);

    close(placement(&server, "door").0, glam::Vec3::ZERO);
    assert_eq!(
        counter_value(&server, "order"),
        2.0,
        "OnFullyOpen must be queued before OnOpen"
    );
}

/// Valve's, reproduced: `DoorHitTop` arms the wait with `SetMoveDoneTime(0)`,
/// which is an alarm that can never fire, and the door is taken out of the
/// simulation list with it. Four `func_door_rotating`s in the shipped game
/// carry `wait 0` and stand open for ever.
#[test]
fn a_door_with_a_wait_of_zero_stays_open_for_ever() {
    let map = vec![block(&[
        ("classname", "func_door_rotating"),
        ("targetname", "door"),
        ("model", "*1"),
        ("distance", "90"),
        ("speed", "90"),
        ("wait", "0"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    run(&mut server, 1.1);
    close(
        placement(&server, "door").1,
        glam::Vec3::new(0.0, 90.0, 0.0),
    );
    assert_eq!(server.thinks.len(), 0, "out of the simulation list");

    run(&mut server, 10.0);
    assert_eq!(
        placement(&server, "door").1,
        glam::Vec3::new(0.0, 90.0, 0.0),
        "and it never closes"
    );
}

/// The travel is the model's own size along `movedir`, less the lip, less the
/// two units Valve subtracts for the engine's bbox expansion.
#[test]
fn the_travel_is_the_model_minus_the_lip_minus_two() {
    let map = vec![block(&[
        ("classname", "func_door"),
        ("targetname", "door"),
        ("model", "*1"),
        ("movedir", "0 90 0"),
        ("lip", "3"),
        ("speed", "100"),
        ("wait", "-1"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    run(&mut server, 1.0);

    // The slab is 10 thick along Y, less Valve's 2, less the lip of 3.
    close(placement(&server, "door").0, glam::Vec3::new(0.0, 5.0, 0.0));
}

// ---------------------------------------------------------------------------
// the depot
// ---------------------------------------------------------------------------

/// The 36 key names nothing consumes, across all 106 shipped maps, with how
/// often each appears.
///
/// This is the parse-side status in one table, and it is worth reading rather
/// than skipping:
///
/// - **19 are the map compiler's.** `_light`, `_lightHDR`, `_quadratic_attn`
///   and the rest of the falloff family are read by `vbsp`/`vrad` at compile
///   time (`utils/vbsp/map.cpp`) and have **no run-time consumer in Valve's
///   engine either** — the shipped server drops them exactly as this one does.
///   `detailvbsp` is `vbsp`'s, `paintinmap` is read by `engine/cmodel.cpp` and
///   `mapversion` appears nowhere in the tree at all. Stage 3 added two more
///   of them by implementing `func_brush` and the doors: `_minlight` is
///   `utils/vrad/radial.cpp:676` and `vrad_brush_cast_shadows` is
///   `utils/vrad/trace.cpp:727`.
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
/// - **`inputfilter` (2,497) is declared by the FGD and consumed by nothing
///   in the entire tree.** `base.fgd:247` gives `func_brush` an `Inputfilter`
///   base class with an `InputFilter` choices key, and there is no
///   `"inputfilter"` string anywhere in `legacy/` — not in `game/server/`, not
///   in the engine, not in the tools. Hammer writes it onto every `func_brush`
///   and nothing has ever read it. The best single example of
///   `portdocs/SERVER.md` §1.4's "the FGD is not an oracle".
/// - **Three more arrived with stage 3's classes and are `CBaseEntity`'s
///   rather than missing**: `health` (682, on every door and button —
///   `m_iHealth`, which needs a damage system), `filtername` (1, on a
///   `func_door` that has no such key in any version of the server — the
///   classes that do are the triggers and `filter_*`), and `message` (3, on
///   `func_door_rotating`, which likewise has no `message` key; `func_rotating`
///   does and its three are consumed).
/// - The rest are genuinely not implemented yet, and all of them are small:
///   `vscripts` (`portdocs/SERVER.md` §9), `SunSpreadAngle`, `ambient`.
///
/// A new name appearing here is a regression; a name leaving it is progress.
/// Either way this table changes and the change should be deliberate.
const EXPECTED_UNHANDLED: &[(&str, usize)] = &[
    ("//onstarttouch", 2),
    ("//ontrigger", 2),
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
    ("_minlight", 169),
    ("_onmapspawn", 1),
    ("_ontrigger", 21),
    ("_quadratic_attn", 7121),
    ("_zero_percent_distance", 4292),
    ("addonpoints", 1),
    ("ambient", 2),
    ("detailvbsp", 106),
    ("filtername", 1),
    ("health", 682),
    ("inputfilter", 2497),
    ("mapversion", 106),
    ("message", 3),
    ("npcpoints", 1),
    ("onendtouchblueplayer", 1),
    ("onendtouchorangeplayer", 1),
    ("onfullyopen", 2),
    ("onproxyrelay", 135),
    ("onstarttouchblueplayer", 1),
    ("onstarttouchorangeplayer", 1),
    ("ontrigger", 15),
    ("onunpressed", 2),
    ("paintinmap", 25),
    ("skin", 1),
    ("sunspreadangle", 27),
    ("vrad_brush_cast_shadows", 2456),
    ("vscripts", 38),
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
    // Stage 3's metric: brush entities, and how many of them are somewhere
    // other than where the entity lump put them once the map has run.
    let mut brush_entities = 0;
    let mut moved = 0;
    let mut still_moving = 0;
    // Stage 4's parse-side metric: how many of the game's brush entities are
    // triggers this port has a class for, and therefore how much of a map is
    // now able to notice the player.
    let mut triggers = 0;

    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let stats = server.level_init(name, &bsp.entities(), &bsp.models);

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

        // `ClientPutInServer`. The player joins after the map's own entities,
        // as it does in `Scene::load`, so that `!player` resolves for the
        // 1,640 connections in the game that name it. It does not *move* here
        // — the touch pass needs collision, which is
        // `every_shipped_maps_triggers_notice_the_player`'s job — so what this
        // adds is the I/O half: `point_teleport`, `Kill`, and every other
        // input aimed at `!player`.
        let spawn = bsp
            .entities()
            .iter()
            .find(|e| e.classname() == Some("info_player_start"))
            .and_then(|e| e.vector("origin"))
            .unwrap_or(glam::Vec3::ZERO);
        server.spawn_player(player_at(spawn));

        for (_, entity) in server.entities.iter() {
            if entity
                .core
                .is_solid_flag_set(crate::server::movement::FSOLID_TRIGGER)
            {
                triggers += 1;
            }
        }

        // Where every brush entity starts, so that the run below can be asked
        // whether anything actually moved.
        brush_entities += server.brush_entity_count();
        let placed: Vec<(usize, glam::Vec3, glam::Vec3)> = (0..bsp.models.len())
            .filter_map(|i| {
                let entity = server.brush_entity(i)?;
                Some((i, entity.origin, entity.angles))
            })
            .collect();

        // …and now run it. Nothing here may panic, and the think list must not
        // grow without bound.
        let interval = server.time().interval;
        let ticks = (RUN_SECONDS / interval).round() as u32;
        for _ in 0..ticks {
            server.frame(interval, &mut NoTouchQuery);
            peak_thinks = peak_thinks.max(server.thinks.len());
        }

        for (index, origin, angles) in placed {
            let Some(entity) = server.brush_entity(index) else {
                continue;
            };
            if entity.origin != origin || entity.angles != angles {
                moved += 1;
            }
            if entity.will_simulate_game_physics() {
                still_moving += 1;
            }
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
    println!(
        "    {brush_entities} brush entities have a class; \
         {moved} of them are not where the lump put them, \
         {still_moving} are still moving; \
         {triggers} are live triggers"
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

    // The parse side. Stage 1 matched 17,069 blocks and spawned 10,132,
    // stage 2 took it to 19,229 and 12,292, stage 3's six brush classes were
    // 3,410 more of both, and stage 4's twelve — five triggers, six filters
    // and `point_teleport` — are 3,322 more again.
    assert_eq!(total.matched, 25_961);
    assert_eq!(total.spawned, 19_024);
    assert_eq!(total.outputs, 53_155);
    assert_eq!(total.unknown.len(), 167);
    assert_eq!(total.unknown.values().sum::<usize>(), 34_964);
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
    // Stage 3's. `func_brush` is the third commonest classname in the game,
    // behind `logic_relay` and `prop_dynamic`.
    assert_eq!(per_class.get("func_brush"), Some(&2_502));
    assert_eq!(per_class.get("func_door_rotating"), Some(&346));
    assert_eq!(per_class.get("func_door"), Some(&275));
    assert_eq!(per_class.get("func_movelinear"), Some(&196));
    assert_eq!(per_class.get("func_button"), Some(&64));
    assert_eq!(per_class.get("func_rotating"), Some(&27));
    // Stage 4's. `trigger_once` is the fifth commonest classname in the game.
    assert_eq!(per_class.get("trigger_once"), Some(&1_476));
    assert_eq!(per_class.get("trigger_multiple"), Some(&899));
    assert_eq!(per_class.get("trigger_hurt"), Some(&215));
    assert_eq!(per_class.get("trigger_push"), Some(&192));
    assert_eq!(per_class.get("trigger_teleport"), Some(&110));
    assert_eq!(per_class.get("point_teleport"), Some(&128));
    assert_eq!(per_class.get("filter_activator_class"), Some(&212));
    assert_eq!(per_class.get("filter_activator_name"), Some(&74));
    assert_eq!(per_class.get("filter_multi"), Some(&9));
    assert_eq!(per_class.get("filter_player_held"), Some(&4));
    assert_eq!(per_class.get("filter_damage_type"), Some(&2));
    assert_eq!(per_class.get("filter_activator_model"), Some(&1));
    // …and **no `player`**: the class is registered because Valve registers
    // it, and no shipped map places one. The 106 in the list are the ones
    // `spawn_player` put there, counted after this loop.
    assert_eq!(per_class.get("player"), None);

    // 105 of the 106 maps place a tone mapper; `sp_a5_credits` is the one that
    // does not.
    assert_eq!(maps_with_a_master, 105);
    // …and 100 of those 105 have set a custom ceiling within two seconds. The
    // five that have not are maps whose controller is driven by something
    // later than the bootstrap — a trigger the player has to walk into.
    assert_eq!(custom_max, 100);

    // The run side. These are what two seconds of every shipped map does.
    assert_eq!(io.dispatched, 5_766);
    assert_eq!(io.accepted, 2_480);
    assert_eq!(io.thinks, 1_450);
    // Most events reach nothing because most *targets* are entities of classes
    // this port has not got — `prop_dynamic` alone is 8,072 of them. Expect
    // this number to fall as classes land.
    assert_eq!(io.no_target, 2_548);

    // Nothing may fail to convert: every shipped connection's parameter is
    // compatible with the input it is aimed at.
    assert_eq!(io.bad_conversion, 0, "a shipped map has a bad I/O link");

    // The whole set of inputs that reach an implemented class and are refused.
    //
    // **Nine names, and 1,078 of the 1,081 occurrences are the parenting
    // family.** `func_brush` alone takes 883 `SetParentAttachmentMaintainOffset`
    // in the first two seconds of the game, because a Hammer instance parents
    // its clip brushes to a moving platform and the `logic_auto` bootstrap is
    // what does the parenting. That family stays unimplemented on purpose and
    // the condition is a real one: `SetParent` needs a local/abs transform
    // pair on `EntityCore`, which this port does not have (a child's origin is
    // the world-space one the map gave and nothing rebases it), and
    // `SetParentAttachment*` needs `LookupAttachment` on a studio model, which
    // would be this module's first dependency on `studio/`. Three of the
    // remaining names are the player procedurals (stage 5's) and one is
    // `RunScriptCode` (`portdocs/SERVER.md` §9).
    let unhandled: Vec<(&str, usize)> =
        io.unhandled.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        unhandled,
        vec![
            ("!player_blue (no such player)", 37),
            ("!player_orange (no such player)", 37),
            ("func_brush.SetParent", 12),
            ("func_brush.SetParentAttachmentMaintainOffset", 883),
            ("info_target.SetParent", 1),
            ("info_target.SetParentAttachment", 12),
            ("info_target.SetParentAttachmentMaintainOffset", 1),
            ("logic_relay.RunScriptCode", 1),
            ("player.SetFogController", 97),
            ("trigger_hurt.SetParentAttachmentMaintainOffset", 19),
            ("trigger_multiple.SetParentAttachmentMaintainOffset", 2),
        ],
        "the set of inputs nothing handles has changed"
    );

    // Stage 3, end to end: every brush entity the port has a class for, and
    // how many of them the first two seconds of the game actually move.
    //
    // 67 is not a large number and it is the right one: a Portal 2 map starts
    // with its doors shut, and what moves in the first two seconds is the
    // handful of panels and lifts a chamber opens with. What matters is that
    // it is not **zero** — before stage 3 nothing in any map moved at all —
    // and that 34 of them are still in flight when the clock stops, so the
    // simulation list is being entered and left rather than filled once.
    //
    // **Stage 4 doubled the brush-entity count and moved nothing extra**, and
    // both halves are the point: the five trigger classes are 2,892 more brush
    // entities the game now answers for, and not one of them is a mover.
    assert_eq!(brush_entities, 6_302);
    assert_eq!(moved, 67, "brush entities that left their spawn placement");
    assert_eq!(still_moving, 34, "…and were still travelling at 2s");

    // Stage 4's own parse-side number: of those 6,302, how many are *live*
    // triggers two ticks into the map — `FSOLID_TRIGGER` set, so the touch
    // pass will look at them. 2,892 triggers are placed and 637 of them are
    // `StartDisabled`, including **107 of the game's 110 `trigger_teleport`s**.
    // `every_shipped_maps_triggers_notice_the_player` then walks a player into
    // every one.
    assert_eq!(triggers, 2_255);

    // `ThinkList` is a flat `Vec` with a linear scan, which is only the right
    // shape while this number is small. It is the measurement `think.rs` cites.
    //
    // **Stage 3 put every moving entity in this list and the peak did not
    // change**, which is the measurement `think.rs` said to retake: a mover is
    // only in it while it is actually travelling, and the 43 is set by the
    // `logic_auto` bootstrap rather than by anything that moves. Stage 4 takes
    // it to 48 — a `trigger_multiple` holds a think for its whole `wait`, and
    // a `trigger_once` for the tenth of a second before it deletes itself.
    assert_eq!(peak_thinks, 48);

    // The one map this port looks at most, and the headline of the whole
    // stage: `sp_a1_intro1` asks for a ceiling of 1.5 against the cvar default
    // of 2, through `logic_relay @rl_lighting_fixup`'s `OnSpawn` →
    // `@rl_prestasis_exposure_reload` → the controller. Its sibling
    // `@rl_poststasis_exposure_reload` is triggered by the same `OnSpawn` and
    // asks for 5; it is `StartDisabled 1`, so honouring that key is what
    // decides which number the map gets.
    let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("the intro map parses");
    server.level_init("sp_a1_intro1", &bsp.entities(), &bsp.models);
    let interval = server.time().interval;
    for _ in 0..(RUN_SECONDS / interval).round() as u32 {
        server.frame(interval, &mut NoTouchQuery);
    }
    let settings = server.tonemap_settings();
    assert!(settings.use_custom_auto_exposure_max);
    assert_eq!(settings.custom_auto_exposure_max, 1.5);
    assert_eq!(settings.custom_auto_exposure_min, 1.0);
    assert_eq!(settings.rate, 0.25);
}

// ===========================================================================
// stage 4 — touch, triggers, filters and the player
// ===========================================================================

/// A [`TouchQuery`] over axis-aligned boxes in world space.
///
/// The real one sweeps the toucher's hull against each brush model's own
/// brushes (`World::brush_models_touching`, tested against a real map in
/// `engine::world`). This is the *shape* of that answer with none of the
/// geometry, which is what a test about the touch link list wants: the
/// question here is what happens once two things overlap, not which two do.
struct BoxTriggers {
    /// `("*N" index, world-space mins, world-space maxs)`.
    boxes: Vec<(usize, Vec3, Vec3)>,
}

impl BoxTriggers {
    fn new(boxes: &[(usize, Vec3, Vec3)]) -> BoxTriggers {
        BoxTriggers {
            boxes: boxes.to_vec(),
        }
    }
}

impl TouchQuery for BoxTriggers {
    fn brush_models_touching(
        &mut self,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        out: &mut Vec<usize>,
    ) {
        // The swept hull's own bounds, which is a conservative stand-in for a
        // real sweep and exact for the axis-aligned motion these tests use.
        let sweep_min = start.min(end) + mins;
        let sweep_max = start.max(end) + maxs;
        for &(index, min, max) in &self.boxes {
            let overlaps = (0..3).all(|i| sweep_min[i] <= max[i] && sweep_max[i] >= min[i]);
            if overlaps {
                out.push(index);
            }
        }
    }
}

/// Runs `seconds` of server time with a touch query in place.
fn run_touching(server: &mut Server, query: &mut dyn TouchQuery, seconds: f32) {
    let interval = server.time().interval;
    let ticks = (seconds / interval).round() as u32;
    ticks_touching(server, query, ticks);
}

/// The same, counted in ticks — for the tests where "one tick" is the point.
fn ticks_touching(server: &mut Server, query: &mut dyn TouchQuery, ticks: u32) {
    let interval = server.time().interval;
    for _ in 0..ticks {
        server.frame(interval, query);
    }
}

/// A player hull, standing, at `origin`.
fn player_at(origin: Vec3) -> PlayerState {
    PlayerState {
        origin,
        angles: Vec3::ZERO,
        velocity: Vec3::ZERO,
        base_velocity: Vec3::ZERO,
        on_ground: true,
        noclip: false,
        mins: Vec3::new(-16.0, -16.0, 0.0),
        maxs: Vec3::new(16.0, 16.0, 72.0),
    }
}

/// The bounding box a `BoxTriggers` entry uses for model `*1` in these tests:
/// a room-sized volume around the origin.
fn trigger_box() -> (Vec3, Vec3) {
    (Vec3::new(-64.0, -64.0, 0.0), Vec3::new(64.0, 64.0, 128.0))
}

/// A map with one trigger of `classname`, named `zone`, wired to a counter.
fn trigger_map(classname: &str, output: &str, extra: &[(&str, &str)]) -> Vec<bsp::Entity> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("classname", classname),
        ("targetname", "zone"),
        ("model", "*1"),
        // `SF_TRIGGER_ALLOW_CLIENTS`, which 1,220 of the game's 1,476
        // `trigger_once`s carry (as 4097, with Hammer's default mass bit).
        ("spawnflags", "1"),
    ];
    pairs.extend_from_slice(extra);
    let conn_value = conn("count", "Add", "1", "0", "-1");
    let mut trigger = block(&pairs);
    trigger.pairs.push((output.to_owned(), conn_value));

    vec![
        block(&[("classname", "worldspawn")]),
        trigger,
        block(&[("classname", "math_counter"), ("targetname", "count")]),
    ]
}

/// Two models: the world, and a 128-unit cube for the trigger.
fn trigger_models() -> Vec<bsp::Model> {
    vec![
        model([-512.0, -512.0, -512.0], [512.0, 512.0, 512.0]),
        model([-64.0, -64.0, 0.0], [64.0, 64.0, 128.0]),
    ]
}

/// The stage, in one test: the player walks into a volume and the map notices.
#[test]
fn walking_into_a_trigger_fires_its_outputs() {
    let map = trigger_map("trigger_multiple", "OnStartTouch", &[("wait", "1")]);
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::new(1000.0, 0.0, 0.0)));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);

    // Outside: nothing.
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(counter_value(&server, "count"), 0.0);

    // Inside.
    server.set_player_state(player_at(Vec3::ZERO));
    run_touching(&mut server, &mut query, 0.1);
    assert_eq!(counter_value(&server, "count"), 1.0);
}

/// `SF_TRIGGER_ALLOW_CLIENTS` is what makes a trigger notice a player, and a
/// trigger without it notices nothing — 141 of the game's `trigger_multiple`s
/// are physics-only in exactly this way.
#[test]
fn a_trigger_that_does_not_allow_clients_ignores_the_player() {
    let mut map = trigger_map("trigger_multiple", "OnStartTouch", &[]);
    // Spawnflag 8 alone: `SF_TRIGGER_ALLOW_PHYSICS`.
    map[1]
        .pairs
        .iter_mut()
        .find(|(k, _)| k == "spawnflags")
        .expect("spawnflags")
        .1 = String::from("8");

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(counter_value(&server, "count"), 0.0);
}

/// `CTriggerOnce::Spawn`'s one line — `m_flWait = -1` — sends
/// `ActivateMultiTrigger` down the branch that stops touching and schedules
/// `SUB_Remove` 0.1 s later.
#[test]
fn a_trigger_once_fires_once_and_then_deletes_itself() {
    let map = trigger_map("trigger_once", "OnTrigger", &[]);
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);

    run_touching(&mut server, &mut query, 0.05);
    assert_eq!(counter_value(&server, "count"), 1.0);
    assert!(server.brush_entity(1).is_some(), "still there for 0.1s");

    run_touching(&mut server, &mut query, 0.5);
    assert_eq!(counter_value(&server, "count"), 1.0, "and only once");
    assert!(
        server.brush_entity(1).is_none(),
        "SUB_Remove ran a tenth of a second later"
    );
}

/// A `trigger_multiple`'s re-trigger lock-out **is the think schedule**:
/// `if ( GetNextThink() > gpGlobals->curtime ) return`.
#[test]
fn a_trigger_multiple_re_arms_after_its_wait() {
    let map = trigger_map("trigger_multiple", "OnTrigger", &[("wait", "1")]);
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);

    run_touching(&mut server, &mut query, 0.9);
    assert_eq!(counter_value(&server, "count"), 1.0, "once inside the wait");
    run_touching(&mut server, &mut query, 0.3);
    assert_eq!(counter_value(&server, "count"), 2.0, "and again after it");
}

/// `OnEndTouch` and `OnEndTouchAll` are driven by the touch **stamp**, not by
/// a second geometric test: a link the tick did not restamp is a touch that
/// ended.
#[test]
fn leaving_a_trigger_fires_on_end_touch() {
    let mut map = trigger_map("trigger_multiple", "OnStartTouch", &[("wait", "1")]);
    map[1].pairs.push((
        String::from("OnEndTouch"),
        conn("count", "Add", "10", "0", "-1"),
    ));
    map[1].pairs.push((
        String::from("OnEndTouchAll"),
        conn("count", "Add", "100", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.1);
    assert_eq!(counter_value(&server, "count"), 1.0);

    server.set_player_state(player_at(Vec3::new(1000.0, 0.0, 0.0)));
    run_touching(&mut server, &mut query, 0.1);
    assert_eq!(
        counter_value(&server, "count"),
        111.0,
        "OnEndTouch and OnEndTouchAll, in the same tick"
    );
}

/// Only one side of a touch owes an `EndTouch`, and it is the trigger's.
#[test]
fn only_the_trigger_side_of_a_touch_carries_the_start_flag() {
    let map = trigger_map("trigger_multiple", "OnStartTouch", &[]);
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    let player = server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.05);

    let trigger = find_named(&server, "zone");
    assert_eq!(trigger.touch_links.len(), 1);
    assert!(
        trigger.touch_links[0].start_touch,
        "the trigger fired StartTouch and owes an EndTouch"
    );

    let player = server.entities.get(player).expect("the player");
    assert_eq!(player.touch_links.len(), 1);
    assert!(
        !player.touch_links[0].start_touch,
        "the other side of the link is a trigger, so the player owes nothing"
    );
}

/// A `trigger_once` deleting itself fires **no** `OnEndTouch`, because
/// `PhysicsRemoveTouchedList` frees its own links rather than removing them.
#[test]
fn a_trigger_that_deletes_itself_fires_no_end_touch() {
    let mut map = trigger_map("trigger_once", "OnTrigger", &[]);
    map[1].pairs.push((
        String::from("OnEndTouch"),
        conn("count", "Add", "10", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.5);

    assert!(server.brush_entity(1).is_none(), "it went");
    assert_eq!(
        counter_value(&server, "count"),
        1.0,
        "OnTrigger only — the OnEndTouch never fires"
    );
}

/// `Enable`/`Disable` move `FSOLID_TRIGGER`, which is what the touch pass
/// filters on — so a disabled trigger is simply not asked about.
#[test]
fn a_disabled_trigger_notices_nothing_until_it_is_enabled() {
    let map = trigger_map(
        "trigger_multiple",
        "OnStartTouch",
        &[("wait", "1"), ("StartDisabled", "1")],
    );
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(counter_value(&server, "count"), 0.0);

    let zone = find_named(&server, "zone").id();
    server.accept_input(zone, "Enable", Variant::Void, None, None, 0);
    run_touching(&mut server, &mut query, 0.1);
    assert_eq!(counter_value(&server, "count"), 1.0);
}

/// The filter that 250 of the game's `trigger_multiple`s wear: cubes only, so
/// a player walking through does nothing.
#[test]
fn a_class_filter_keeps_the_player_out() {
    let mut map = trigger_map(
        "trigger_multiple",
        "OnStartTouch",
        &[("wait", "1"), ("filtername", "cubes")],
    );
    map.push(block(&[
        ("classname", "filter_activator_class"),
        ("targetname", "cubes"),
        ("filterclass", "prop_weighted_cube"),
        ("Negated", "Allow entities that match criteria"),
    ]));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(
        counter_value(&server, "count"),
        0.0,
        "the player is not a cube"
    );
}

/// …and negating it lets everything *but* a cube through. Hammer writes the
/// choices label rather than the number, and `atoi` of a label is 0 — three of
/// the game's 74 name filters carry a literal `1` instead.
#[test]
fn a_negated_filter_is_the_other_way_round() {
    let mut map = trigger_map(
        "trigger_multiple",
        "OnStartTouch",
        &[("wait", "1"), ("filtername", "not_cubes")],
    );
    map.push(block(&[
        ("classname", "filter_activator_class"),
        ("targetname", "not_cubes"),
        ("filterclass", "prop_weighted_cube"),
        ("Negated", "1"),
    ]));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(counter_value(&server, "count"), 1.0);
}

/// `filter_multi` is the one class in the game whose handler reads *other*
/// entities while it is being dispatched — the condition `rustdocs/SERVER.md`
/// said would change `Context`'s shape, and it did.
#[test]
fn filter_multi_combines_its_children() {
    // AND( name is "!player", class is not "prop_weighted_cube" ).
    let mut map = trigger_map(
        "trigger_multiple",
        "OnStartTouch",
        &[("wait", "1"), ("filtername", "both")],
    );
    map.push(block(&[
        ("classname", "filter_multi"),
        ("targetname", "both"),
        ("FilterType", "0"),
        ("Filter01", "is_player"),
        ("Filter02", "not_a_cube"),
    ]));
    map.push(block(&[
        ("classname", "filter_activator_name"),
        ("targetname", "is_player"),
        ("filtername", "!player"),
    ]));
    map.push(block(&[
        ("classname", "filter_activator_class"),
        ("targetname", "not_a_cube"),
        ("filterclass", "prop_weighted_cube"),
        ("Negated", "1"),
    ]));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(counter_value(&server, "count"), 1.0, "both children pass");

    // …and an OR of a passing and a failing child also passes, where an AND
    // would not: swap one child for one that refuses everything.
    let mut map = trigger_map(
        "trigger_multiple",
        "OnStartTouch",
        &[("wait", "1"), ("filtername", "either")],
    );
    map.push(block(&[
        ("classname", "filter_multi"),
        ("targetname", "either"),
        ("FilterType", "1"),
        ("Filter01", "is_player"),
        ("Filter02", "is_a_cube"),
    ]));
    map.push(block(&[
        ("classname", "filter_activator_name"),
        ("targetname", "is_player"),
        ("filtername", "!player"),
    ]));
    map.push(block(&[
        ("classname", "filter_activator_class"),
        ("targetname", "is_a_cube"),
        ("filterclass", "prop_weighted_cube"),
    ]));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.2);
    assert_eq!(counter_value(&server, "count"), 1.0);
}

/// **121 of the game's 128 `point_teleport`s target `!player`**, so this is
/// the class the player entity pays for.
#[test]
fn point_teleport_sends_the_player_where_it_was_told() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "point_teleport"),
            ("targetname", "go"),
            ("target", "!player"),
            ("origin", "512 256 64"),
            ("angles", "0 90 0"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    server.spawn_player(player_at(Vec3::ZERO));

    let go = find_named(&server, "go").id();
    server.accept_input(go, "Teleport", Variant::Void, None, None, 0);

    let state = server.player_state().expect("a player");
    close(state.origin, Vec3::new(512.0, 256.0, 64.0));
    close(state.angles, Vec3::new(0.0, 90.0, 0.0));
}

/// A `trigger_teleport` with a landmark carries the toucher's *offset* across
/// rather than dropping it on the destination — which is how the elevator
/// between chapters works, and how 37 of the game's 110 are set up.
#[test]
fn a_landmark_teleport_carries_the_offset_across() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_teleport"),
            ("targetname", "zone"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("target", "there"),
            ("landmark", "here"),
        ]),
        // The landmark and the destination, 1,000 units apart and both facing
        // the same way, so the offset survives unrotated.
        block(&[
            ("classname", "info_target"),
            ("targetname", "here"),
            ("origin", "0 0 0"),
        ]),
        block(&[
            ("classname", "info_target"),
            ("targetname", "there"),
            ("origin", "1000 0 0"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::new(10.0, 20.0, 0.0)));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    // **One tick.** A `trigger_teleport` teleports on every `Touch`, so a
    // player left standing in one is moved every tick — which is the shipped
    // behaviour and is why the 37 elevator teleports in the game are
    // `StartDisabled` and fire once.
    ticks_touching(&mut server, &mut query, 1);

    let state = server.player_state().expect("a player");
    close(state.origin, Vec3::new(1010.0, 20.0, 0.0));

    // …and the next tick does not sweep the 1,000 units it just crossed: the
    // teleport reset the swept-from point, so nothing between here and there
    // is touched on the way.
    ticks_touching(&mut server, &mut query, 1);
    close(
        server.player_state().expect("a player").origin,
        Vec3::new(1010.0, 20.0, 0.0),
    );
}

/// …and with no landmark the toucher lands *on* the destination.
#[test]
fn a_teleport_with_no_landmark_lands_on_its_target() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_teleport"),
            ("targetname", "zone"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("target", "there"),
        ]),
        block(&[
            ("classname", "info_target"),
            ("targetname", "there"),
            ("origin", "1000 0 0"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::new(10.0, 20.0, 0.0)));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    ticks_touching(&mut server, &mut query, 1);

    // `vecPentTargetOrigin.z -= pOther->WorldAlignMins().z` for a player, and
    // Portal 2's standing hull has `mins.z == 0`, so it is a no-op.
    close(
        server.player_state().expect("a player").origin,
        Vec3::new(1000.0, 0.0, 0.0),
    );
}

/// `trigger_push` sets a base velocity every tick it is pushing, and the tick
/// after the player leaves, `CheckMovingGround` turns it into real velocity
/// with a `1 + frametime/2` boost.
#[test]
fn a_push_is_a_base_velocity_and_then_momentum() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_push"),
            ("targetname", "blower"),
            ("model", "*1"),
            ("spawnflags", "1"),
            // `AngleVectors( "0 0 0" )` is `+X`.
            ("pushdir", "0 0 0"),
            ("speed", "150"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.05);

    // **Twice the map's number**: `CTriggerPush::Activate`'s Portal 2
    // single-player doubling.
    let state = server.player_state().expect("a player");
    close(state.base_velocity, Vec3::new(300.0, 0.0, 0.0));
    close(state.velocity, Vec3::ZERO);

    // Step out. The base velocity survives one more tick — the flag from the
    // last touch is still set when `CheckMovingGround` looks — and becomes
    // momentum on the one after.
    server.set_player_state(player_at(Vec3::new(1000.0, 0.0, 0.0)));
    run_touching(&mut server, &mut query, 0.05);
    let state = server.player_state().expect("a player");
    close(state.base_velocity, Vec3::ZERO);
    let interval = server.time().interval;
    close(
        state.velocity,
        Vec3::new(300.0 * (1.0 + interval * 0.5), 0.0, 0.0),
    );
}

/// A noclipping player is not pushed. `CTriggerPush::Touch`'s switch has
/// `MOVETYPE_NOCLIP` fall straight out.
#[test]
fn a_noclipping_player_is_not_pushed() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_push"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("pushdir", "0 0 0"),
            ("speed", "150"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    let mut state = player_at(Vec3::ZERO);
    state.noclip = true;
    server.spawn_player(state);
    server.set_player_state(state);

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.1);
    close(
        server.player_state().expect("a player").base_velocity,
        Vec3::ZERO,
    );
}

/// `trigger_hurt` fires its outputs on Valve's half-second cadence and takes
/// nothing away, because there is no health — see `classes::TriggerHurt`.
#[test]
fn a_hurt_trigger_fires_on_hurt_player_twice_a_second() {
    let mut map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_hurt"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("damage", "20"),
        ]),
        block(&[("classname", "math_counter"), ("targetname", "count")]),
    ];
    map[1].pairs.push((
        String::from("OnHurtPlayer"),
        conn("count", "Add", "1", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    // `Touch` arms the think at `curtime`, and the touch pass runs *before*
    // the thinks in the same tick — so the first dose lands immediately, and
    // then one every half second.
    run_touching(&mut server, &mut query, 1.1);
    assert_eq!(counter_value(&server, "count"), 3.0);

    // > **Walking out charges nothing extra, and that is Valve's.**
    // > `EndTouch`'s parting half-dose is gated on the toucher not being in
    // > `m_hurtEntities`, and that list is only cleared at the *start* of a
    // > `HurtAllTouchers` — so anyone who has been hurt at all this cycle is
    // > in it. With one toucher and a think that fires on the same tick as
    // > the first touch, the branch is unreachable. See
    // > [`a_radiation_trigger_charges_on_the_way_out`] for the shape that
    // > does reach it.
    //
    // [`a_radiation_trigger_charges_on_the_way_out`]: fn@a_radiation_trigger_charges_on_the_way_out
    server.set_player_state(player_at(Vec3::new(1000.0, 0.0, 0.0)));
    run_touching(&mut server, &mut query, 0.1);
    assert_eq!(counter_value(&server, "count"), 3.0);
}

/// The one shape that reaches `CTriggerHurt::EndTouch`'s parting dose, and
/// the reason it is worth having: a **radiation** trigger already has a think,
/// so `Touch` does not arm the half-second one and a quick walk through would
/// otherwise be free.
///
/// 27 of the game's 215 `trigger_hurt`s set `DMG_RADIATION`.
#[test]
fn a_radiation_trigger_charges_on_the_way_out() {
    let mut map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_hurt"),
            ("model", "*1"),
            ("spawnflags", "1"),
            ("damage", "20"),
            // `DMG_RADIATION`.
            ("damagetype", "262144"),
        ]),
        block(&[("classname", "math_counter"), ("targetname", "count")]),
    ];
    map[1].pairs.push((
        String::from("OnHurtPlayer"),
        conn("count", "Add", "1", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::new(1000.0, 0.0, 0.0)));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    // Long enough outside that a `RadiationThink` has run `HurtAllTouchers`
    // against an empty trigger and cleared the hurt list.
    run_touching(&mut server, &mut query, 1.0);
    assert_eq!(counter_value(&server, "count"), 0.0);

    // In for one tick — no immediate dose, because the think is the radiation
    // one and is already armed.
    server.set_player_state(player_at(Vec3::ZERO));
    ticks_touching(&mut server, &mut query, 1);
    assert_eq!(counter_value(&server, "count"), 0.0);

    // …and out again, which is where the half-dose lands. **Two ticks**: the
    // first still touches, because the sweep from the old origin to the new
    // one crosses the trigger — which is the anti-tunnelling property doing
    // its job rather than a quirk of the test.
    server.set_player_state(player_at(Vec3::new(1000.0, 0.0, 0.0)));
    ticks_touching(&mut server, &mut query, 2);
    assert_eq!(counter_value(&server, "count"), 1.0);
}

/// The player is in the entity list, so `!player` resolves and the 1,640
/// connections in the game that name it reach something.
#[test]
fn the_player_is_an_entity_and_resolves_procedurally() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[("classname", "info_target"), ("targetname", "spot")]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    assert!(server.player().is_none(), "no client, no player");

    let player = server.spawn_player(player_at(Vec3::ZERO));
    assert_eq!(server.player(), Some(player));
    assert!(
        server
            .entities
            .get(player)
            .expect("alive")
            .behaviour
            .is_player(),
        "IsPlayer()"
    );

    // `Kill` at `!player` removes it, which two shipped connections send.
    let spot = find_named(&server, "spot").id();
    {
        let time = server.time();
        let Server {
            entities,
            queue,
            random,
            ..
        } = &mut server;
        let mut cx = Context::new(time, queue, random, entities, Some(player));
        cx.post_named("!player", "Kill", Variant::Void, 0.0, None, Some(spot), 0);
    }
    run(&mut server, 0.1);
    assert!(server.player().is_none(), "the handle stopped resolving");
}

/// Where a standing player's **feet** have to be for its hull to be inside
/// brush model `index`'s actual brushes, or `None`.
///
/// Feet, not the box centre: `Player::origin` is the feet and the hull runs
/// 72 units up from there, so a probe that answered in centres would place
/// every player half a hull too high — which is exactly the mistake this
/// comment exists to stop, and which cost thirteen thin triggers on the first
/// run.
///
/// The bounding-box centre first, because for a plain box trigger — most of
/// them — that is it in one test; then a 5×5×5 lattice inset into the box.
/// Used only by the depot test below, to answer "where would a player have to
/// be" without hand-annotating two thousand triggers.
#[cfg(test)]
fn probe_inside(
    collision: &crate::engine::trace::CollisionBsp,
    placed: &[crate::engine::world::PlacedBrushModel],
    index: usize,
    mins: Vec3,
    maxs: Vec3,
) -> Option<Vec3> {
    use crate::engine::trace::{Contents, Ray};

    let model = placed.iter().find(|p| p.index == index)?.model;
    let mut tracer = collision.tracer();
    let (hull_min, hull_max) = (Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0));

    let mut candidates = vec![(mins + maxs) * 0.5];
    const STEPS: i32 = 5;
    for i in 0..STEPS {
        for j in 0..STEPS {
            for k in 0..STEPS {
                let t = |n: i32| (n as f32 + 0.5) / STEPS as f32;
                candidates.push(mins + (maxs - mins) * Vec3::new(t(i), t(j), t(k)));
            }
        }
    }

    candidates
        .into_iter()
        // The candidates are box centres, so the feet are half a hull lower.
        .map(|centre| centre - Vec3::new(0.0, 0.0, 36.0))
        .find(|&feet| {
            let ray = Ray::hull(feet, feet, hull_min, hull_max);
            let trace = tracer.trace_model(&ray, &model, Contents::MASK_SOLID);
            trace.contents.intersects(Contents::MASK_SOLID)
        })
}

/// **Every trigger in the shipped game, touched by a real player hull swept
/// against its real brushes.**
///
/// The stage's headline measurement, and the one that could not be faked: it
/// builds each map's collision, spawns a player, and walks it into the centre
/// of every trigger the port has a class for — through
/// [`World::brush_models_touching`](crate::engine::world::World::brush_models_touching),
/// the same `ClipRayToCollideable` the running game uses — then asks whether
/// the trigger noticed.
///
/// Three things fail loudly here and are invisible to every synthetic test
/// above: a wrong `"*N"` join, a trigger whose `FSOLID_TRIGGER` never got set,
/// and a swept-box test that answers for the model's *bounding box* rather
/// than its brushes (a test chamber's triggers are L-shaped often enough that
/// the two disagree).
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release triggers_notice -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_maps_triggers_notice_the_player() {
    use crate::engine::trace::CollisionBsp;
    use crate::engine::world::{bsp::Bsp, find_brush_models, PlacedBrushModel};

    /// The engine's half of the touch query, over a map's placed brush models.
    /// `engine/mod.rs`'s `WorldTouchQuery` without a `World` around it — the
    /// depot test has no GPU and so cannot build one — and **the same
    /// function body**, which is why `world/` exposes it free.
    struct Placed<'a> {
        collision: &'a CollisionBsp,
        models: &'a [PlacedBrushModel],
    }

    impl TouchQuery for Placed<'_> {
        fn brush_models_touching(
            &mut self,
            start: Vec3,
            end: Vec3,
            mins: Vec3,
            maxs: Vec3,
            out: &mut Vec<usize>,
        ) {
            crate::engine::world::brush_models_touching(
                self.collision,
                self.models,
                start,
                end,
                mins,
                maxs,
                out,
            );
        }
    }

    let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
        panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
    };
    let dir = std::path::PathBuf::from(dir);
    let base = dir.parent().unwrap_or(&dir).to_path_buf();
    let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
        .expect("mount the game");

    let mut names: Vec<String> = vfs
        .list("maps")
        .expect("maps/")
        .into_iter()
        .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
        .map(|e| e.name.trim_end_matches(".bsp").to_owned())
        .collect();
    names.sort();

    let (mut visited, mut noticed, mut fired) = (0usize, 0usize, 0usize);
    // A trigger with no point a standing player fits in, and one the map's own
    // bootstrap switched off or deleted before the second tick.
    let (mut unreachable, mut withdrawn) = (0usize, 0usize);
    let mut by_class: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();

    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let collision = CollisionBsp::build(&bsp);
        let entities = bsp.entities();
        let placed = find_brush_models(&entities, &collision);

        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);

        // Every trigger the port has a class for, with the world-space box of
        // the brush model it names.
        let mut targets: Vec<(usize, &'static str, Vec3, Vec3)> = Vec::new();
        for (_, entity) in server.entities.iter() {
            if !entity
                .core
                .is_solid_flag_set(crate::server::movement::FSOLID_TRIGGER)
            {
                continue;
            }
            let Some(index) = entity
                .core
                .model
                .as_deref()
                .and_then(|m| m.strip_prefix('*'))
                .and_then(|n| n.parse::<usize>().ok())
            else {
                continue;
            };
            let bounds = entity.core.model_bounds;
            targets.push((
                index,
                entity.classname(),
                entity.core.origin + bounds.mins,
                entity.core.origin + bounds.maxs,
            ));
        }

        // Only the models the server owns go into the query — the same rule
        // `World::clip_models` follows.
        // …with `owned` set, which is what `sync_brush_models` does in the
        // running engine and what the filter above reads.
        let owned: Vec<PlacedBrushModel> = placed
            .iter()
            .filter(|p| server.brush_entity(p.index).is_some())
            .cloned()
            .map(|mut p| {
                p.owned = true;
                p
            })
            .collect();

        for (index, classname, mins, maxs) in targets {
            // **A fresh level per trigger.** Triggers overlap, and a
            // `trigger_once` fired by standing in its neighbour deletes itself
            // a tenth of a second later — so a shared server visits some of
            // them after they have gone. That is correct behaviour and a
            // useless measurement.
            server.level_init(name, &entities, &bsp.models);

            visited += 1;
            let entry = by_class.entry(classname).or_default();
            entry.0 += 1;

            // A point inside the trigger's *brushes*, which is not the same as
            // a point inside its bounding box: a Portal 2 trigger is often two
            // slabs either side of a doorway, or an L around a corner, and the
            // box centre of one of those is empty air. The probe is the same
            // sweep the touch pass uses, aimed at this one model.
            let Some(probe) = probe_inside(&collision, &placed, index, mins, maxs) else {
                unreachable += 1;
                continue;
            };

            let before = server.io.dispatched;
            server.spawn_player(player_at(probe));
            let mut query = Placed {
                collision: &collision,
                models: &owned,
            };
            // Two ticks, and a touch on **either** counts: a
            // `trigger_teleport` moves the player out of itself on the first
            // one, so its link is gone by the second.
            let interval = server.time().interval;
            server.frame(interval, &mut query);
            let touched = server
                .brush_entity(index)
                .is_some_and(|e| !e.touch_links.is_empty());
            server.frame(interval, &mut query);
            let touched = touched
                || server
                    .brush_entity(index)
                    .is_some_and(|e| !e.touch_links.is_empty());

            if touched {
                noticed += 1;
                entry.1 += 1;
            } else {
                // The map's own bootstrap can take a trigger away before the
                // second tick: a `logic_relay`'s `OnSpawn` fires one tick in,
                // and a `Kill` or a `Disable` on the other end of it is
                // ordinary level design. Distinguish that from a failure to
                // notice.
                match server.brush_entity(index) {
                    // The map killed the player — two of them do, through a
                    // `logic_relay`'s `OnSpawn` and a `point_teleport`.
                    _ if server.player().is_none() => withdrawn += 1,
                    None => withdrawn += 1,
                    Some(e) if !e.is_solid_flag_set(crate::server::movement::FSOLID_TRIGGER) => {
                        withdrawn += 1
                    }
                    Some(_) => println!("    MISS {name} *{index} {classname} at {probe:?}"),
                }
            }
            if server.io.dispatched > before {
                fired += 1;
            }
        }
    }

    println!("\n{} maps", names.len());
    println!(
        "  {visited} triggers visited, {noticed} noticed the player, \
         {fired} of them fired something;\n  \
         {unreachable} had no point a standing player fits in, \
         {withdrawn} were switched off or deleted by the map before the second tick"
    );
    for (classname, (visited, noticed)) in &by_class {
        println!("    {noticed:>5} of {visited:>5}  {classname}");
    }

    assert_eq!(names.len(), 106);
    // Every trigger a player can physically stand in must notice one standing
    // in it. There is no room for a partial answer: a miss is a wrong `"*N"`
    // join, a missing `FSOLID_TRIGGER`, or a touch link that never formed.
    assert_eq!(
        noticed + unreachable + withdrawn,
        visited,
        "a trigger did not notice a player standing inside it"
    );

    // The exact census, so that a change has to be read rather than absorbed.
    //
    // **2,255 live triggers** is the 2,892 the maps place minus the 637 that
    // are `StartDisabled` — 1,371 `trigger_once` of 1,476, 702
    // `trigger_multiple` of 899, 142 `trigger_hurt` of 215, 37 `trigger_push`
    // of 192 and 3 `trigger_teleport` of 110. **107 of the game's 110
    // teleports start switched off**, which is what makes an elevator an
    // elevator rather than a trap.
    assert_eq!(visited, 2_255);
    assert_eq!(noticed, 2_246);
    // 1,879 of them get as far as dispatching something, which is the whole
    // chain — geometry, `FSOLID_TRIGGER`, the touch link, `PassesTriggerFilters`
    // and an output with a connection on it. The 367 that do not are triggers
    // whose outputs go to entities this port has no class for, or whose filter
    // says "cubes only".
    assert_eq!(fired, 1_879);
    // Three triggers in the game have no point a 32x32x72 hull fits inside.
    assert_eq!(unreachable, 3);
    // …and six are switched off, deleted, or take the player with them within
    // two ticks of the map starting.
    assert_eq!(withdrawn, 6);
    assert!(visited > 2_000, "only {visited} triggers visited");
}

/// **Every brush class must set a solid *type*, and the ones that are solid
/// must end up in the clip chain.**
///
/// This is the test the runtime found: stage 4 gave `EntityCore` a `Solid`
/// and set it in `InitTrigger` and on the player, and the five stage-3 brush
/// classes were left at `SOLID_NONE` — so `is_solid()` was false for every
/// door in the game, `World::clip_models` was empty, and the clip chain
/// silently collided with nothing. Every unit test still passed, because they
/// all build a `PlacedBrushModel` by hand.
#[test]
fn every_brush_class_is_solid_unless_it_says_otherwise() {
    /// One row of the table below: a classname, the keys to add to it, whether
    /// it should end up with a solid *type*, and whether `is_solid()`.
    type Case = (
        &'static str,
        &'static [(&'static str, &'static str)],
        bool,
        bool,
    );

    const CASES: &[Case] = &[
        ("func_door", &[], true, true),
        ("func_door_rotating", &[], true, true),
        ("func_movelinear", &[], true, true),
        // `SF_MOVELINEAR_NOTSOLID`, which 92 of the game's 196 set: it keeps
        // `SOLID_VPHYSICS` and adds the bit.
        ("func_movelinear", &[("spawnflags", "8")], true, false),
        ("func_button", &[], true, true),
        // `SF_BUTTON_NOTSOLID` — zero of the game's 64, and **the one place
        // anything here chooses `SOLID_NONE`**, which is why this case exists
        // at all.
        ("func_button", &[("spawnflags", "16384")], false, false),
        ("func_rotating", &[], true, true),
        // `SF_ROTATING_NOT_SOLID`, which 19 of the game's 27 set.
        ("func_rotating", &[("spawnflags", "64")], true, false),
        ("func_brush", &[], true, true),
        // `Solidity` **1** is `BRUSHSOLID_NEVER`; 2 is `BRUSHSOLID_ALWAYS`.
        ("func_brush", &[("Solidity", "1")], true, false),
        ("func_brush", &[("Solidity", "2")], true, true),
        // …and `StartDisabled` is `TurnOff`, which is the same bit.
        ("func_brush", &[("StartDisabled", "1")], true, false),
        // A trigger: `SOLID_BSP` *and* `FSOLID_NOT_SOLID`, so it has a
        // collision model the touch query can sweep and stops nobody.
        ("trigger_once", &[], true, false),
        ("trigger_multiple", &[], true, false),
        ("trigger_hurt", &[], true, false),
        ("trigger_push", &[], true, false),
        ("trigger_teleport", &[], true, false),
    ];

    for &(classname, extra, expect_type, expect_solid) in CASES {
        let mut pairs: Vec<(&str, &str)> = vec![("classname", classname), ("model", "*1")];
        pairs.extend_from_slice(extra);
        let map = vec![block(&[("classname", "worldspawn")]), block(&pairs)];

        let mut server = Server::new();
        server.level_init("test", &map, &trigger_models());
        let entity = server.brush_entity(1).expect("the brush entity");

        assert_eq!(
            entity.solid != crate::server::movement::Solid::None,
            expect_type,
            "{classname} {extra:?}: a missing solid type is invisible to \
             `World::clip_models`, so nothing collides and nothing complains"
        );
        assert_eq!(
            entity.is_solid(),
            expect_solid,
            "{classname} {extra:?}: is_solid()"
        );
        // …and a trigger is the only thing here that is *also* a trigger.
        assert_eq!(
            entity.is_solid_flag_set(crate::server::movement::FSOLID_TRIGGER),
            classname.starts_with("trigger_"),
            "{classname} {extra:?}: FSOLID_TRIGGER"
        );
    }
}
