//! Stage 1's tests: the spawn pipeline against synthetic maps, and against the
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
            ("OnTrigger", "door\u{1b}Open\u{1b}\u{1b}0\u{1b}-1"),
            ("OnTrigger", "panel\u{1b}Close\u{1b}\u{1b}1.5\u{1b}1"),
            ("OnUser1", "other\u{1b}Kill\u{1b}\u{1b}0\u{1b}-1"),
            ("OnUnPressed", "junk\u{1b}Kill\u{1b}\u{1b}0\u{1b}-1"),
        ]),
        block(&[
            ("classname", "func_instance_io_proxy"),
            ("targetname", "proxy"),
            (
                "OnProxyRelay1",
                "start_relay\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1",
            ),
            ("OnProxyRelay", "unfixed\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1"),
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
    assert_eq!(relay.outputs.len(), 3, "two OnTrigger and one OnUser1");
    assert_eq!(relay.outputs[0].0, "OnTrigger");
}

fn find<'a>(server: &'a Server, classname: &str) -> &'a Entity {
    server
        .entities
        .iter()
        .map(|(_, e)| e)
        .find(|e| e.classname() == classname)
        .unwrap_or_else(|| panic!("no {classname} in the list"))
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
// the depot
// ---------------------------------------------------------------------------

/// The 28 key names nothing consumes, across all 106 shipped maps, with how
/// often each appears.
///
/// This is stage 1's status in one table, and it is worth reading rather than
/// skipping:
///
/// - **17 are the map compiler's.** `_light`, `_lightHDR`, `_quadratic_attn`
///   and the rest of the falloff family are read by `vbsp`/`vrad` at compile
///   time (`utils/vbsp/map.cpp`) and have **no run-time consumer in Valve's
///   engine either** — the shipped server drops them exactly as this one does.
///   `detailvbsp` is `vbsp`'s, `paintinmap` is read by `engine/cmodel.cpp` and
///   `mapversion` appears nowhere in the tree at all.
/// - **4 are mapper mistakes shipped in the game**: a `logic_relay` with a key
///   called `//OnTrigger`, seventeen with `_OnTrigger`, two with
///   `OnUnPressed`, and a `light_spot` with `AddonPoints`/`NpcPoints` from a
///   different entity's FGD.
/// - **`OnProxyRelay` (135)** is the unnumbered output the FGD shows a mapper;
///   Hammer's instance compiler is supposed to rewrite it into a numbered one
///   and these were missed. No version of the server has ever handled it.
/// - The rest are genuinely not implemented yet, and all of them are small:
///   `vscripts` (stage 2 at the earliest), `SunSpreadAngle`, `ambient`.
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
    ("_ontrigger", 17),
    ("_quadratic_attn", 7121),
    ("_zero_percent_distance", 4292),
    ("addonpoints", 1),
    ("ambient", 2),
    ("detailvbsp", 106),
    ("mapversion", 106),
    ("npcpoints", 1),
    ("onproxyrelay", 135),
    ("ontrigger", 4),
    ("onunpressed", 2),
    ("paintinmap", 25),
    ("sunspreadangle", 27),
    ("vscripts", 1),
];

/// Every shipped map's entity lump, spawned for real.
///
/// Ignored by default and gated on `KISAK_GAME_DIR`, like the `world/`,
/// `studio/` and `trace/` depot tests. Needs no GPU: the `.bsp` is read for
/// its entity lump and nothing else.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_maps -- --ignored --nocapture
/// ```
///
/// The numbers are exact, because the files are. What earns the runtime is
/// **`CLight::Spawn`**: of the 7,150 light entities the game places, 6,937
/// have no `targetname` and delete themselves, and the 213 that do keep one
/// survive. A port that skipped that one `if` would carry eleven per cent of
/// the game's entity list as garbage and every other number here would still
/// be right.
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_map_spawns_its_entities() {
    use crate::engine::world::bsp::Bsp;
    use crate::filesystem::Vfs;
    use std::collections::BTreeMap;

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
    let mut per_class: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut named_lights = 0;

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

    assert_eq!(names.len(), 106, "Portal 2 ships 106 maps");
    assert_eq!(total.blocks, 60_925);
    assert_eq!(total.matched, 17_069);
    assert_eq!(total.spawned, 10_132);
    assert_eq!(total.removed_on_spawn, 6_937, "unnamed lights");
    assert_eq!(
        named_lights, 213,
        "lights that survive because they are named"
    );
    assert_eq!(total.outputs, 42_065);
    assert_eq!(total.unknown.len(), 191);
    assert_eq!(total.unknown.values().sum::<usize>(), 43_856);
    // Only `info_target` among the implemented classes is ever parented, and
    // **none of the 25 resolves**, which is a fact about stage 1 rather than a
    // failure: 21 of them name an entity of a class that is not implemented
    // yet (13 a `prop_dynamic`, 3 a `func_rotating`, 2 a `func_tracktrain`,
    // and one each a `prop_dynamic_override`, a `func_physbox` and a
    // `func_movelinear`) and **the other 4 name an entity that does not exist
    // in the map at all** — mapper errors shipped in the game, which will
    // still not resolve when everything else does. Expect this number to fall
    // to 4 and stop.
    assert_eq!((total.parented, total.parents_missing), (25, 25));

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
    // live list rather than from the lump.
    assert_eq!(per_class.get("logic_relay"), Some(&8_082));
    assert_eq!(per_class.get("func_instance_io_proxy"), Some(&1_184));
    assert_eq!(per_class.get("info_target"), Some(&431));
    assert_eq!(per_class.get("info_player_start"), Some(&116));
    assert_eq!(per_class.get("worldspawn"), Some(&106));
}
