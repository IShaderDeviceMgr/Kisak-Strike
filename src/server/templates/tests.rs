use super::*;

fn block(pairs: &[(&str, &str)]) -> bsp::Entity {
    bsp::Entity {
        pairs: pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
    }
}

fn entry(pairs: &[(&str, &str)]) -> TemplateEntry {
    TemplateEntry {
        block: block(pairs),
        entity_to_template: Mat4::IDENTITY,
        needs_fixup: false,
    }
}

fn value<'a>(entry: &'a TemplateEntry, key: &str) -> Vec<&'a str> {
    entry
        .block
        .pairs
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

/// `Templates_ReconnectIOForGroup`: a connection from one member to another
/// names it with the fixup, the named member's own name gets it too, and a
/// name from outside the group is left alone.
#[test]
fn names_inside_a_group_are_marked_for_fixup_and_names_outside_are_not() {
    let mut entries = vec![
        entry(&[
            ("classname", "logic_relay"),
            ("targetname", "relay"),
            ("OnTrigger", "cube\u{1b}Dissolve\u{1b}\u{1b}0\u{1b}-1"),
            ("OnTrigger", "outside\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1"),
        ]),
        entry(&[("classname", "prop_weighted_cube"), ("targetname", "Cube")]),
        entry(&[("classname", "info_target"), ("targetname", "alone")]),
    ];
    reconnect_io(&mut entries, true);
    assert_eq!(
        value(&entries[0], "OnTrigger"),
        ["cube&0000\u{1b}Dissolve\u{1b}\u{1b}0\u{1b}-1", "outside\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1"]
    );
    assert_eq!(value(&entries[1], "targetname"), ["Cube&0000"]);
    // The relay names another member but is not named by one, so its own
    // name stays — and both ends of the connection need fixing.
    assert_eq!(value(&entries[0], "targetname"), ["relay"]);
    assert!(entries[0].needs_fixup && entries[1].needs_fixup);
    assert!(!entries[2].needs_fixup);
}

/// Spawnflag 2: *"only one instance of the entities will ever be spawned at a
/// time"*, so nothing is renamed.
#[test]
fn a_template_that_preserves_names_changes_nothing() {
    let mut entries = vec![
        entry(&[("targetname", "a"), ("target", "b")]),
        entry(&[("targetname", "b")]),
    ];
    let before: Vec<_> = entries.iter().map(|e| e.block.pairs.clone()).collect();
    reconnect_io(&mut entries, false);
    let after: Vec<_> = entries.iter().map(|e| e.block.pairs.clone()).collect();
    assert_eq!(after, before);
    assert!(entries.iter().all(|e| !e.needs_fixup));
}

/// `Templates_GetEntityIOFixedMapData`: every `&dddd` becomes the instance's
/// own, and an `&` that is not followed by four digits is left alone.
#[test]
fn an_instance_stamps_its_number_over_every_fixup() {
    let fixed = with_instance_names(
        &block(&[("targetname", "cube&0000"), ("OnFizzled", "relay&0000\u{1b}Trigger&Co")]),
        "&0007",
    );
    assert_eq!(fixed.pairs[0].1, "cube&0007");
    assert_eq!(fixed.pairs[1].1, "relay&0007\u{1b}Trigger&Co");
}

// ---------------------------------------------------------------------------
// Through the server
// ---------------------------------------------------------------------------

fn named(server: &Server, wanted: &str) -> Vec<EntityId> {
    name::find_by_name(&server.entities, wanted).collect()
}

fn connection(target: &str, input: &str) -> String {
    format!("{target}\u{1b}{input}\u{1b}1\u{1b}0\u{1b}-1")
}

fn fire(server: &mut Server, target: &str, input: &str) {
    let id = named(server, target)[0];
    server.accept_input(id, input, Variant::Void, None, None, 0);
}

fn counter(server: &Server) -> f32 {
    server
        .entities
        .get(named(server, "count")[0])
        .and_then(|e| e.behaviour.downcast_ref::<classes::MathCounter>())
        .expect("a counter")
        .value
}

/// **A template's entity leaves the map at load and comes back where it was
/// when the template is asked for.** The template stands 100 units above the
/// target it names and is turned 90°; the target's placement is kept in the
/// template's frame, so `ForceSpawn` — which makes the instance at the
/// template's own placement — puts it back exactly where the map had it.
#[test]
fn a_templated_entity_is_gone_at_load_and_made_by_force_spawn() {
    let spawned = connection("count", "Add");
    let mut server = Server::new();
    let stats = server.level_init(
        "test",
        &[
            block(&[("classname", "worldspawn")]),
            block(&[("classname", "math_counter"), ("targetname", "count")]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "thing"),
                ("origin", "10 0 0"),
            ]),
            block(&[
                ("classname", "point_template"),
                ("targetname", "template"),
                ("origin", "0 0 100"),
                ("angles", "0 90 0"),
                ("spawnflags", "2"),
                ("Template01", "thing"),
                ("OnEntitySpawned", spawned.as_str()),
            ]),
        ],
        &[],
    );
    assert_eq!(stats.templated, 1);
    assert!(named(&server, "thing").is_empty(), "gone at load");

    fire(&mut server, "template", "ForceSpawn");
    let made = named(&server, "thing");
    assert_eq!(made.len(), 1);
    let at = server.entities.get(made[0]).unwrap().core.origin;
    assert!((at - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-3, "{at}");
    let interval = server.time().interval;
    server.frame(interval, &mut crate::server::NoTouchQuery);
    assert_eq!(counter(&server), 1.0, "OnEntitySpawned");

    // Twice more: names are preserved, so there are three `thing`s now.
    fire(&mut server, "template", "ForceSpawn");
    fire(&mut server, "template", "ForceSpawn");
    assert_eq!(named(&server, "thing").len(), 3);
}

/// Without spawnflag 2 each instance gets its own names, and a connection
/// inside the group goes to the member of the *same* instance. Only a member
/// that something in the group *names* is renamed — the counter here, not
/// the relay.
#[test]
fn each_instance_of_a_fixed_up_template_talks_to_itself() {
    let trigger_other = connection("counter_in_group", "Add");
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[("classname", "worldspawn")]),
            block(&[
                ("classname", "logic_relay"),
                ("targetname", "relay_in_group"),
                ("OnTrigger", trigger_other.as_str()),
            ]),
            block(&[("classname", "math_counter"), ("targetname", "counter_in_group")]),
            block(&[
                ("classname", "point_template"),
                ("targetname", "template"),
                ("spawnflags", "0"),
                ("Template01", "relay_in_group"),
                ("Template02", "counter_in_group"),
            ]),
        ],
        &[],
    );
    fire(&mut server, "template", "ForceSpawn");
    let first: Vec<EntityId> = named(&server, "relay_in_group");
    fire(&mut server, "template", "ForceSpawn");
    assert_eq!(named(&server, "relay_in_group").len(), 2, "the relay keeps its name");
    assert_eq!(named(&server, "counter_in_group&0001").len(), 1);
    assert_eq!(named(&server, "counter_in_group&0002").len(), 1);

    let second = named(&server, "relay_in_group")
        .into_iter()
        .find(|id| !first.contains(id))
        .expect("the second instance's relay");
    server.accept_input(second, "Trigger", Variant::Void, None, None, 0);
    let interval = server.time().interval;
    server.frame(interval, &mut crate::server::NoTouchQuery);
    let value = |name: &str| {
        server
            .entities
            .get(named(&server, name)[0])
            .and_then(|e| e.behaviour.downcast_ref::<classes::MathCounter>())
            .unwrap()
            .value
    };
    assert_eq!(value("counter_in_group&0002"), 1.0, "its own instance's counter");
    assert_eq!(value("counter_in_group&0001"), 0.0, "and not the other's");
}

/// A templated brush entity's model is hidden until it is made, and placed by
/// the new entity once it is.
#[test]
fn a_templated_brush_model_is_hidden_until_made() {
    let model = |mins: [f32; 3], maxs: [f32; 3]| bsp::Model {
        mins,
        maxs,
        origin: [0.0; 3],
        head_node: 0,
        first_face: 0,
        num_faces: 0,
    };
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[("classname", "worldspawn")]),
            block(&[("classname", "func_brush"), ("targetname", "wall"), ("model", "*1")]),
            block(&[
                ("classname", "point_template"),
                ("targetname", "template"),
                ("spawnflags", "2"),
                ("Template01", "wall"),
            ]),
        ],
        &[model([0.0; 3], [0.0; 3]), model([-8.0; 3], [8.0; 3])],
    );
    assert!(server.is_templated_brush_model(1));
    assert!(server.brush_entity(1).is_none());
    fire(&mut server, "template", "ForceSpawn");
    let wall = server.brush_entity(1).expect("the made wall places *1");
    assert_eq!(wall.model_bounds.maxs, Vec3::splat(8.0));
}
