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
        // Two blocks of a classname the port does not implement. It was
        // `prop_dynamic` until that landed; `ambient_generic` is the
        // replacement because sound is the furthest-away subsystem there is.
        block(&[("classname", "ambient_generic"), ("message", "a.wav")]),
        block(&[("classname", "ambient_generic"), ("message", "b.wav")]),
    ]
}

#[test]
fn a_map_becomes_an_entity_list() {
    let mut server = Server::new();
    let stats = server.level_init("test", &sample(), &[]);

    assert_eq!(stats.blocks, 8);
    assert_eq!(stats.matched, 6, "ambient_generic is not implemented yet");
    assert_eq!(stats.unknown.get("ambient_generic"), Some(&2));
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
    assert!(world.parent().is_none());
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
    assert_eq!(server.entities.get(child).unwrap().parent(), Some(root));
    assert_eq!(
        server.entities.get(grandchild).unwrap().parent(),
        Some(child)
    );
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
    assert!(find(&server, "info_target").parent().is_none());
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

/// The map places a child at its **world** position and `parentname` rebases
/// it — `SetupParentsForSpawnList` running before the spawn pass is what makes
/// this the state a `Spawn` sees.
#[test]
fn a_maps_parentname_rebases_the_child_into_its_parents_frame() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[
                ("classname", "info_target"),
                ("targetname", "arm"),
                ("origin", "100 0 0"),
                ("angles", "0 90 0"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "rider"),
                ("origin", "100 40 0"),
                ("parentname", "arm"),
            ]),
        ],
        &[],
    );

    let rider = find_named(&server, "rider");
    assert_eq!(
        rider.origin,
        Vec3::new(100.0, 40.0, 0.0),
        "where Hammer put it"
    );
    // The arm is yawed a quarter turn, so "40 units along +Y of the world" is
    // "40 units along the arm's own +X".
    assert!(
        (rider.local_origin - Vec3::new(40.0, 0.0, 0.0)).length() < 1e-3,
        "{:?}",
        rider.local_origin
    );
    assert_eq!(
        find_named(&server, "arm").children().len(),
        1,
        "and the parent names it back"
    );
}

/// The whole point of the pair: a mover that carries something.
///
/// A `func_movelinear` with an `info_target` bolted to it, opened by an input
/// — the rider ends up exactly as far along as the platform without ever being
/// touched.
#[test]
fn a_mover_carries_what_is_parented_to_it() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[
                ("classname", "func_movelinear"),
                ("targetname", "lift"),
                ("model", "*1"),
                ("origin", "0 0 0"),
                ("movedir", "-90 0 0"),
                ("movedistance", "128"),
                ("speed", "100"),
                ("startposition", "0"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "rider"),
                ("origin", "0 0 64"),
                ("parentname", "lift"),
            ]),
        ],
        &[],
    );

    let lift = name::find_by_name(&server.entities, "lift").next().unwrap();
    assert!(server.accept_input(lift, "Open", Variant::Void, None, None, 0));
    // 128 units at 100 a second, plus a tick for the alarm to be armed.
    ticks_touching(&mut server, &mut NoTouchQuery, 90);

    let lift = find_named(&server, "lift");
    let rider = find_named(&server, "rider");
    assert!(
        (lift.origin.z - 128.0).abs() < 1e-3,
        "the platform arrived: {:?}",
        lift.origin
    );
    assert!(
        (rider.origin.z - 192.0).abs() < 1e-3,
        "…and took the rider with it: {:?}",
        rider.origin
    );
    // The rider never moved in the frame it lives in, which is the invariant
    // that makes this work rather than a coincidence of two entities getting
    // the same answer.
    assert_eq!(rider.local_origin, Vec3::new(0.0, 0.0, 64.0));
    assert_eq!(rider.velocity, Vec3::ZERO);
}

/// `InputSetParent`/`InputClearParent`: both hold the world placement still,
/// and the second is the one whose C++ does not look like it does.
#[test]
fn set_parent_and_clear_parent_leave_the_entity_where_it_is() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[
                ("classname", "info_target"),
                ("targetname", "arm"),
                ("origin", "0 0 100"),
                ("angles", "0 45 0"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "rider"),
                ("origin", "50 0 100"),
            ]),
        ],
        &[],
    );
    let rider_id = name::find_by_name(&server.entities, "rider")
        .next()
        .unwrap();
    let placed = find_named(&server, "rider").origin;

    assert!(server.accept_input(
        rider_id,
        "SetParent",
        Variant::String("arm".to_owned()),
        None,
        None,
        0
    ));
    let rider = find_named(&server, "rider");
    assert_eq!(rider.origin, placed, "SetParent moves nothing in the world");
    assert_eq!(
        rider.parent(),
        Some(name::find_by_name(&server.entities, "arm").next().unwrap())
    );
    assert!(
        (rider.local_origin.length() - 50.0).abs() < 1e-3,
        "…but it is 50 units out in a frame yawed 45°: {:?}",
        rider.local_origin
    );

    assert!(server.accept_input(rider_id, "ClearParent", Variant::Void, None, None, 0));
    let rider = find_named(&server, "rider");
    assert!(rider.parent().is_none());
    assert_eq!(rider.origin, placed, "and ClearParent moves nothing either");
    assert_eq!(
        rider.local_origin, placed,
        "the local pair becomes the world one"
    );
    assert!(find_named(&server, "arm").children().is_empty());
}

// ---------------------------------------------------------------------------
// SetParentAttachment — parenting to a point on a bone
// ---------------------------------------------------------------------------

/// An arm with one attachment point, `muzzle`, which sits [`RADIUS`] units out
/// along the model's `+X` and **swings a quarter turn about the model origin
/// over one cycle** of the sequence `swing`.
///
/// Any other sequence holds it at cycle 0, which is what makes "the parent is
/// playing something else" observable. The cycle is derived the way the real
/// table derives it — from `anim_time`, the rate and `now` — because that is
/// the half of [`Posed`](crate::server::attachment::Posed) a stale
/// `m_flCycle` would hide.
struct SwingingArm;

impl SwingingArm {
    const MODEL: &'static str = "models/arm.mdl";
    const RADIUS: f32 = 40.0;
    /// Seconds for one full swing, so `cycle = elapsed / DURATION`.
    const DURATION: f32 = 1.0;
}

impl crate::server::attachment::Attachments for SwingingArm {
    fn lookup(&self, model: &str, name: &str) -> Option<usize> {
        let known = model.eq_ignore_ascii_case(Self::MODEL) && name.eq_ignore_ascii_case("muzzle");
        known.then_some(0)
    }

    fn attachment_to_model(
        &self,
        posed: &crate::server::attachment::Posed<'_>,
        attachment: usize,
    ) -> Option<glam::Affine3A> {
        if !posed.model.eq_ignore_ascii_case(Self::MODEL) || attachment != 0 {
            return None;
        }
        let cycle = match posed.sequence.eq_ignore_ascii_case("swing") {
            true => (posed.cycle
                + (posed.now - posed.anim_time).max(0.0) * posed.playback_rate / Self::DURATION)
                .clamp(0.0, 1.0),
            false => 0.0,
        };
        let turn = cycle * std::f32::consts::FRAC_PI_2;
        Some(
            glam::Affine3A::from_rotation_z(turn)
                * glam::Affine3A::from_translation(Vec3::new(Self::RADIUS, 0.0, 0.0)),
        )
    }
}

/// An arm wearing [`SwingingArm::MODEL`] and a `func_brush` rider parented to
/// it, both at the world origin unless `arm_origin` says otherwise.
fn arm_and_rider(arm_origin: &str, rider_origin: &str) -> Vec<bsp::Entity> {
    vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "prop_dynamic"),
            ("targetname", "arm"),
            ("model", SwingingArm::MODEL),
            ("origin", arm_origin),
        ]),
        block(&[
            ("classname", "func_brush"),
            ("targetname", "rider"),
            ("model", "*1"),
            ("origin", rider_origin),
            ("parentname", "arm"),
        ]),
    ]
}

fn arm_server(arm_origin: &str, rider_origin: &str) -> Server {
    let mut server = Server::new();
    server.level_init("test", &arm_and_rider(arm_origin, rider_origin), &[]);
    server.set_attachments(Box::new(SwingingArm));
    server
}

fn attach(server: &mut Server, input: &str, point: &str) -> bool {
    let rider = name::find_by_name(&server.entities, "rider")
        .next()
        .unwrap();
    server.accept_input(
        rider,
        input,
        Variant::String(point.to_owned()),
        None,
        None,
        0,
    )
}

/// **`SetParentAttachment` snaps the entity onto the point**, which is
/// `SetLocalOrigin( vec3_origin )` after the re-parent — and it sets
/// `MOVETYPE_NONE`, because something pinned to a bone has no business
/// integrating a velocity.
#[test]
fn set_parent_attachment_puts_the_entity_on_the_attachment_point() {
    let mut server = arm_server("0 0 0", "300 0 0");
    assert!(attach(&mut server, "SetParentAttachment", "muzzle"));

    let rider = find_named(&server, "rider");
    assert_eq!(rider.parent_attachment(), Some(0));
    assert_eq!(rider.move_type, crate::server::movement::MoveType::None);
    assert_eq!(rider.local_origin, Vec3::ZERO, "snapped onto the point");
    assert!(
        (rider.origin - Vec3::new(SwingingArm::RADIUS, 0.0, 0.0)).length() < 1e-3,
        "…which is 40 units out along the arm's +X: {:?}",
        rider.origin
    );
}

/// `SetParentAttachmentMaintainOffset` differs in one bool: the local pair is
/// **not** zeroed, so the entity stays where it is in the world and keeps that
/// offset from the point from then on.
#[test]
fn set_parent_attachment_maintain_offset_leaves_the_entity_where_it_was() {
    let mut server = arm_server("0 0 0", "300 0 0");
    let placed = find_named(&server, "rider").origin;
    assert!(attach(
        &mut server,
        "SetParentAttachmentMaintainOffset",
        "muzzle"
    ));

    let rider = find_named(&server, "rider");
    assert_eq!(rider.parent_attachment(), Some(0));
    assert_eq!(rider.origin, placed, "nothing moved in the world");
    assert!(
        (rider.local_origin - Vec3::new(300.0 - SwingingArm::RADIUS, 0.0, 0.0)).length() < 1e-3,
        "…and the offset is now measured from the point: {:?}",
        rider.local_origin
    );
}

/// **The one that is the whole point of the feature.** The arm never moves and
/// never turns; its *animation* moves the bone the point rides, and the rider
/// goes with it.
///
/// Plain parenting cannot express this — the parent's own transform is
/// constant for the whole test — so a rider that stayed put would be a rider
/// that quietly fell back to it.
#[test]
fn an_attachment_child_follows_the_bone_as_the_parent_animates() {
    let mut server = arm_server("0 0 0", "0 0 0");
    let arm = find_named(&server, "arm").id();
    server.accept_input(
        arm,
        "SetAnimation",
        Variant::String("swing".to_owned()),
        None,
        None,
        0,
    );
    assert!(attach(&mut server, "SetParentAttachment", "muzzle"));

    let start = find_named(&server, "rider").origin;
    assert!((start - Vec3::new(SwingingArm::RADIUS, 0.0, 0.0)).length() < 1e-3);

    // Half a swing: the point has turned 45° about the arm's origin, so the
    // rider is 40 units out at 45°.
    run(&mut server, SwingingArm::DURATION / 2.0);
    let rider = find_named(&server, "rider");
    let half = std::f32::consts::FRAC_PI_4;
    let expected = Vec3::new(
        SwingingArm::RADIUS * half.cos(),
        SwingingArm::RADIUS * half.sin(),
        0.0,
    );
    assert!(
        (rider.origin - expected).length() < 1.0,
        "the rider should have swung with the bone: {:?} vs {expected:?}",
        rider.origin
    );

    // The arm itself has not moved at all, which is what makes the above the
    // attachment's doing and not the parent's.
    assert_eq!(find_named(&server, "arm").origin, Vec3::ZERO);
    assert_eq!(find_named(&server, "arm").angles, Vec3::ZERO);

    // …and it is **not thinking either**: no sequence table was handed over,
    // so `CDynamicProp::AnimThink` took its `Lookup::Unknown` branch on the
    // first tick and never re-armed. That is exactly the case the per-tick
    // refresh exists for — see `Server::refresh_attachment_children` — and
    // before it existed this test froze the rider at tick one.
    assert_eq!(
        find_named(&server, "arm").next_think_tick(),
        crate::server::think::TICK_NEVER_THINK,
        "the arm's own think has cancelled itself"
    );
}

/// Both of Valve's guards **return** rather than falling back to plain
/// parenting, and each is a measured slice of the shipped connections: 75 aim
/// at an entity with no parent, and 2 at one whose parent is a brush model.
/// The input is still *accepted* — it is a declared input that ran and warned.
#[test]
fn set_parent_attachment_is_refused_rather_than_downgraded() {
    // No parent at all.
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[("classname", "worldspawn")]),
            block(&[
                ("classname", "func_brush"),
                ("targetname", "rider"),
                ("model", "*1"),
                ("origin", "300 0 0"),
            ]),
        ],
        &[],
    );
    server.set_attachments(Box::new(SwingingArm));
    assert!(attach(&mut server, "SetParentAttachment", "muzzle"));
    let rider = find_named(&server, "rider");
    assert_eq!(rider.parent(), None);
    assert_eq!(rider.origin, Vec3::new(300.0, 0.0, 0.0));

    // A parent, but no attachment by that name — `LookupAttachment` answering
    // zero, which is 183 `DefaultAnim`-shaped map errors' worth of precedent.
    let mut server = arm_server("0 0 0", "300 0 0");
    assert!(attach(&mut server, "SetParentAttachment", "nosuchpoint"));
    let rider = find_named(&server, "rider");
    assert_eq!(rider.parent_attachment(), None);
    assert_eq!(
        rider.origin,
        Vec3::new(300.0, 0.0, 0.0),
        "still on the plain parent"
    );
}

/// `InputSetParent`'s first three lines: "if we had a parent attachment, clear
/// it, because it's no longer valid". So a later `SetParent` really does take
/// the entity off the bone and put it back on the parent's origin.
#[test]
fn set_parent_after_set_parent_attachment_clears_the_attachment() {
    let mut server = arm_server("0 0 0", "300 0 0");
    assert!(attach(&mut server, "SetParentAttachment", "muzzle"));
    assert_eq!(find_named(&server, "rider").parent_attachment(), Some(0));

    let rider_id = find_named(&server, "rider").id();
    assert!(server.accept_input(
        rider_id,
        "SetParent",
        Variant::String("arm".to_owned()),
        None,
        None,
        0
    ));
    let rider = find_named(&server, "rider");
    assert_eq!(rider.parent_attachment(), None);
    // `SetParent` holds the world placement still, so it stays on the point it
    // was snapped to — but it is now measured from the arm, and the next time
    // the arm animates it will not follow.
    assert!(rider.parent().is_some());
    assert_eq!(rider.local_origin, rider.origin);
}

/// A model whose attachment table nobody filled in is a model with no
/// attachments: every `SetParentAttachment` is refused and every parenting is
/// plain. **That is the state of a level's first tick**, because the models are
/// not read until after `level_init`.
#[test]
fn an_attachment_is_refused_while_the_models_are_still_unloaded() {
    let mut server = Server::new();
    server.level_init("test", &arm_and_rider("0 0 0", "300 0 0"), &[]);
    // No `set_attachments`.
    assert!(attach(&mut server, "SetParentAttachment", "muzzle"));
    assert_eq!(find_named(&server, "rider").parent_attachment(), None);
    assert_eq!(
        find_named(&server, "rider").origin,
        Vec3::new(300.0, 0.0, 0.0)
    );
}

/// `UpdateOnRemove`: "Any children still connected are orphans, mark all for
/// delete". A removed parent takes its whole subtree, not just its children.
#[test]
fn removing_a_parent_removes_everything_under_it() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            block(&[("classname", "info_target"), ("targetname", "root")]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "child"),
                ("parentname", "root"),
            ]),
            block(&[
                ("classname", "info_target"),
                ("targetname", "grandchild"),
                ("parentname", "child"),
            ]),
            // Not under the root, so it must survive.
            block(&[("classname", "info_target"), ("targetname", "bystander")]),
        ],
        &[],
    );
    let before = server.entities.len();
    let root = name::find_by_name(&server.entities, "root").next().unwrap();
    assert!(server.accept_input(root, "Kill", Variant::Void, None, None, 0));
    ticks_touching(&mut server, &mut NoTouchQuery, 2);

    assert_eq!(server.entities.len(), before - 3);
    for gone in ["root", "child", "grandchild"] {
        assert!(
            name::find_by_name(&server.entities, gone).next().is_none(),
            "{gone} should have gone with the root"
        );
    }
    assert!(name::find_by_name(&server.entities, "bystander")
        .next()
        .is_some());
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

/// A `logic_branch_listener`, by [`EntityId`] — a free function and not a
/// closure, because a closure that borrows a `&Server` and returns a reference
/// into it cannot name the lifetime that relates them.
fn branch_list_at(server: &Server, id: EntityId) -> &classes::BranchList {
    server
        .entities
        .get(id)
        .expect("a live entity")
        .behaviour
        .downcast_ref::<classes::BranchList>()
        .expect("a BranchList")
}

/// A `logic_branch`'s remembered boolean, by [`EntityId`] — the form the door
/// chain wants, because the branches there are looked up once and then watched
/// across several ticks.
fn branch_value(server: &Server, id: EntityId) -> bool {
    server
        .entities
        .get(id)
        .expect("a live entity")
        .behaviour
        .downcast_ref::<classes::Branch>()
        .expect("a Branch")
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
    // A name nothing anywhere declares, which is getting harder to find: the
    // transform pair took `SetParent`/`ClearParent` and `LookupAttachment`
    // took both `SetParentAttachment*` forms. `SetBodyGroup` is
    // `CBaseAnimating`'s and no class here is one.
    assert!(!server.accept_input(id, "SetBodyGroup", Variant::Void, None, None, 0));
    assert_eq!(
        server.io.unhandled.get("math_counter.SetBodyGroup"),
        Some(&1)
    );
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

/// `logic_branch_listener` is silent at level start, reports only when the
/// **verdict** changes, and hears a `SetValue` that fires no output of its own.
///
/// All three are load-bearing for a test chamber door. The door's listener
/// watches "the map wants this shut" and "the player is not in the doorway";
/// both are driven by `SetValue`, and 130 of the game's 138 doors close
/// through exactly this.
#[test]
fn a_branch_listener_fires_only_when_the_verdict_changes() {
    let counter = |name: &str| {
        block(&[
            ("classname", "math_counter"),
            ("targetname", name),
            ("max", "100"),
        ])
    };
    let map = vec![
        block(&[
            ("classname", "logic_branch"),
            ("targetname", "a"),
            ("InitialValue", "0"),
        ]),
        block(&[
            ("classname", "logic_branch"),
            ("targetname", "b"),
            ("InitialValue", "0"),
        ]),
        block(&[
            ("classname", "logic_branch_listener"),
            ("targetname", "both"),
            ("Branch01", "a"),
            ("Branch02", "b"),
            ("OnAllTrue", &conn("yes", "Add", "1", "0", "-1")),
            ("OnAllFalse", &conn("no", "Add", "1", "0", "-1")),
            ("OnMixed", &conn("mixed", "Add", "1", "0", "-1")),
        ]),
        counter("yes"),
        counter("no"),
        counter("mixed"),
    ];

    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    let a = find_named(&server, "a").id();
    let b = find_named(&server, "b").id();
    let tally = |server: &Server| {
        (
            counter_value(server, "yes"),
            counter_value(server, "no"),
            counter_value(server, "mixed"),
        )
    };

    // **Nothing at level start.** `Spawn` is empty and `Activate` only
    // registers, so a listener whose branches are already all-false says so to
    // nobody. A door whose branches both read "close me" at spawn would
    // otherwise slam shut on the first tick.
    run(&mut server, 1.0);
    assert_eq!(tally(&server), (0.0, 0.0, 0.0));

    // `SetValue` fires neither `OnTrue` nor `OnFalse` — and still reaches the
    // listener, because the notification is guarded by the *change* and not by
    // the output.
    server.accept_input(a, "SetValue", Variant::Bool(true), None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(tally(&server), (0.0, 0.0, 1.0), "one of two is mixed");

    server.accept_input(b, "SetValue", Variant::Bool(true), None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(tally(&server), (1.0, 0.0, 1.0), "both true");

    // Setting a branch to the value it already holds changes nothing, so the
    // listener is not even told — let alone made to re-report.
    server.accept_input(a, "SetValue", Variant::Bool(true), None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(tally(&server), (1.0, 0.0, 1.0));

    // A real change whose *verdict* is unchanged is told and stays quiet.
    server.accept_input(a, "SetValue", Variant::Bool(false), None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(tally(&server), (1.0, 0.0, 2.0));

    server.accept_input(b, "SetValue", Variant::Bool(false), None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(tally(&server), (1.0, 1.0, 2.0), "both false");
}

/// `Test` resets `m_eLastState` first, so it reports whatever it finds even
/// when nothing has moved — and a listener monitoring nothing reports
/// `OnMixed`.
///
/// Neither is reachable from shipped content: no map fires `Test` at a
/// listener, and all 350 `Branch*` keys in the game resolve. Both are pinned
/// because they are the two arms a reimplementation gets backwards.
#[test]
fn test_forces_a_branch_listener_to_report_and_an_empty_one_is_mixed() {
    let counter = |name: &str| {
        block(&[
            ("classname", "math_counter"),
            ("targetname", name),
            ("max", "100"),
        ])
    };
    let map = vec![
        block(&[
            ("classname", "logic_branch"),
            ("targetname", "a"),
            ("InitialValue", "1"),
        ]),
        block(&[
            ("classname", "logic_branch_listener"),
            ("targetname", "one"),
            ("Branch01", "a"),
            ("OnAllTrue", &conn("yes", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "logic_branch_listener"),
            ("targetname", "none"),
            ("OnAllTrue", &conn("yes", "Add", "1", "0", "-1")),
            ("OnMixed", &conn("mixed", "Add", "1", "0", "-1")),
        ]),
        counter("yes"),
        counter("mixed"),
    ];

    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    let one = find_named(&server, "one").id();
    let none = find_named(&server, "none").id();

    run(&mut server, 1.0);
    assert_eq!(counter_value(&server, "yes"), 0.0, "silent at level start");

    server.accept_input(one, "Test", Variant::Void, None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(counter_value(&server, "yes"), 1.0);

    // And again: `Test` forces an output every time, where a branch change
    // would be deduplicated away.
    server.accept_input(one, "Test", Variant::Void, None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(counter_value(&server, "yes"), 2.0);

    // No branches: neither `bOneTrue` nor `bOneFalse`, so `DoTest` falls
    // through both arms into the `else`.
    server.accept_input(none, "Test", Variant::Void, None, None, 0);
    run(&mut server, 0.05);
    assert_eq!(
        (
            counter_value(&server, "yes"),
            counter_value(&server, "mixed")
        ),
        (2.0, 1.0)
    );
}

/// **Every `logic_branch_listener` in the game, registered and driven** — and
/// the number that actually matters: how many test chamber doors shut when
/// their listener goes all-true.
///
/// The class is invisible to the 106-map census, because in the first two
/// seconds of a level **not one branch in the game changes value** — a chamber
/// door closes after the player has walked through it, which is minutes in.
/// So this is the test with teeth: it opens every door in the map, sets every
/// `logic_branch` true, and watches what comes back.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release branch_listener -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_branch_listener_registers_and_shuts_the_doors_it_is_for() {
    use crate::filesystem::Vfs;

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

    let (mut listeners, mut maps_with_one) = (0usize, 0usize);
    // `Branch01`…`Branch16` keys written by the map, against the entities
    // `Activate` actually found. The two must agree exactly: a key that
    // resolves to nothing is a branch the listener will never hear from.
    let (mut keys_written, mut keys_resolved) = (0usize, 0usize);
    // What each live listener had reported by the end. `reported` is the
    // check — every one of them must have moved off `NOT_INIT`, which is only
    // possible if `Activate` registered it and a branch told it something —
    // and `all_true` is information: the sweep sets every branch true, and a
    // listener's own `OnAllTrue` chain routinely sets one of them back.
    let (mut reported, mut all_true) = (0usize, 0usize);
    // Listeners their own map deletes before they can be driven. Three
    // shipped connections fire `Kill` at one.
    let mut killed = 0usize;
    let (mut doors_here, mut doors_locked) = (0usize, 0usize);
    let (mut doors_opened, mut doors_shut) = (0usize, 0usize);

    for name in &names {
        let bsp = crate::engine::world::bsp::Bsp::load(&vfs, name)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let blocks = bsp.entities();

        // The map's own count, straight out of the lump.
        let written = blocks
            .iter()
            .filter(|e| {
                e.pairs.iter().any(|(k, v)| {
                    k.eq_ignore_ascii_case("classname") && v == "logic_branch_listener"
                })
            })
            .flat_map(|e| e.pairs.iter())
            .filter(|(k, v)| {
                let branch = k.len() == 8
                    && k[..6].eq_ignore_ascii_case("Branch")
                    && k[6..].bytes().all(|b| b.is_ascii_digit());
                branch && !v.is_empty()
            })
            .count();

        let mut server = Server::new();
        server.level_init(name, &blocks, &bsp.models);

        let by_class = |server: &Server, class: &str| -> Vec<EntityId> {
            server
                .entities
                .iter()
                .filter(|(_, e)| e.classname() == class)
                .map(|(id, _)| id)
                .collect()
        };
        let here = by_class(&server, "logic_branch_listener");
        if here.is_empty() {
            assert_eq!(written, 0, "{name}: branch keys with no listener");
            continue;
        }
        maps_with_one += 1;
        let listeners_here = here.len();
        listeners += listeners_here;
        keys_written += written;

        // Registration happens in `Activate`, so the key census is taken
        // before anything runs — three shipped connections fire `Kill` at a
        // listener, and one of them lands during the bootstrap below.
        keys_resolved += here
            .iter()
            .map(|&id| branch_list_at(&server, id).branches().len())
            .sum::<usize>();

        // A second of the map's own bootstrap, so that nothing below is
        // racing `logic_auto`.
        run(&mut server, 1.0);
        let here: Vec<EntityId> = here
            .into_iter()
            .filter(|&id| server.entities.get(id).is_some())
            .collect();
        killed += listeners_here - here.len();

        // Open every door, so that a `Close` reaching one is visible.
        let doors = by_class(&server, "prop_testchamber_door");
        doors_here += doors.len();
        doors_locked += doors
            .iter()
            .filter(|&&id| door_at(&server, id).is_locked())
            .count();
        for &door in &doors {
            server.accept_input(door, "Open", Variant::Void, None, None, 0);
        }
        run(&mut server, 1.0);
        let open_now: Vec<EntityId> = doors
            .iter()
            .copied()
            .filter(|&id| door_at(&server, id).is_open())
            .collect();
        doors_opened += open_now.len();

        // Now tell the whole map that every condition it is waiting on is
        // met. Every listener has at least one branch, so every one of them
        // must come back all-true.
        for branch in by_class(&server, "logic_branch") {
            server.accept_input(branch, "SetValue", Variant::Bool(true), None, None, 0);
        }
        run(&mut server, 0.5);

        // Filtered again: a listener's own `OnAllTrue` chain can end in a
        // `Kill` aimed at it, so surviving the bootstrap is not the same as
        // surviving the sweep.
        let mut live_now = 0usize;
        for &id in &here {
            if server.entities.get(id).is_none() {
                continue;
            }
            live_now += 1;
            match branch_list_at(&server, id).state() {
                "not-init" => {}
                "all-true" => {
                    reported += 1;
                    all_true += 1;
                }
                _ => reported += 1,
            }
        }
        killed += here.len() - live_now;
        doors_shut += open_now
            .iter()
            .filter(|&&id| !door_at(&server, id).is_open())
            .count();
    }

    println!(
        "logic_branch_listener across {} maps:\n  \
         {listeners} listeners on {maps_with_one} maps\n  \
         {keys_written} Branch* keys written, {keys_resolved} resolved\n  \
         {reported} of {live} reported a verdict, {all_true} of them all-true \
         ({killed} killed by their own map)\n  \
         {doors_here} chamber doors on those maps, {doors_locked} locked at spawn; \
         {doors_opened} opened, {doors_shut} shut again",
        names.len(),
        live = listeners - killed
    );

    assert_eq!(listeners, 158);
    assert_eq!(maps_with_one, 46);
    // **Every key resolves, to exactly one entity.** No empty slot, no
    // wildcard, nothing named that is not a `logic_branch` — which is why
    // `Context::find_all_by_name` needs no `FindEntityByClassname` fallback.
    assert_eq!(keys_written, 350);
    assert_eq!(keys_resolved, 350);
    // **The check.** A listener that never moved off `NOT_INIT` either failed
    // to register in `Activate` or was never told a branch had changed, and
    // either one is the bug that leaves a chamber door open for ever.
    assert_eq!(
        reported,
        158 - killed,
        "every live listener heard its branches"
    );
    assert_eq!(
        all_true, 56,
        "the rest were talked back down by their own map"
    );
    // The payoff, and the reason the class was worth porting before anything
    // else: this is how a test chamber shuts behind you.
    // **99 of the game's 138 chamber doors are on one of these 46 maps** —
    // the other 39 are on maps with no branch listener at all, and shut (if
    // they shut) through some other chain. All 99 take an `Open`, and **79 of
    // them are shut again by a listener going all-true**, which is the whole
    // point of the class.
    assert_eq!(doors_here, 99);
    assert_eq!(doors_locked, 0, "none is locked at spawn");
    assert_eq!(doors_opened, 99);
    assert_eq!(doors_shut, 79);
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

// ---------------------------------------------------------------------------
// prop_weighted_cube
// ---------------------------------------------------------------------------

/// A cube, and whatever is worth reading back off it.
fn cube<'a>(server: &'a Server, name: &str) -> &'a classes::WeightedCube {
    find_named(server, name)
        .behaviour
        .downcast_ref::<classes::WeightedCube>()
        .expect("a WeightedCube")
}

fn cube_map(pairs: &[(&str, &str)]) -> Vec<bsp::Entity> {
    let mut keys: Vec<(&str, &str)> = vec![
        ("classname", "prop_weighted_cube"),
        ("targetname", "cube"),
        ("origin", "0 0 0"),
    ];
    keys.extend_from_slice(pairs);
    vec![block(&keys)]
}

/// **The `skin` key is a cube *type*, not a skin**, and that is the single
/// most surprising thing about this class.
///
/// `ConvertOldSkins`' table, every row of it, including the two that collide:
/// the old list had a "Standard Activated" entry at 2 that the type list does
/// not, so everything from 2 up shifts down one and skins 1 and 2 both mean
/// "companion". Getting the decrement wrong gives 70 of the game's 98 cubes
/// the wrong model, and gives it to them *plausibly* — every value still maps
/// to some real cube.
#[test]
fn an_old_skin_key_is_read_as_a_cube_type_and_shifted() {
    // (skin, expected model, expected recomputed skin)
    let cases = [
        (0, "models/props/metal_box.mdl", 0),
        (1, "models/props/metal_box.mdl", 1),
        // The collision: 2 is "Standard Activated" in the old list and becomes
        // a companion cube, exactly as 1 does.
        (2, "models/props/metal_box.mdl", 1),
        (3, "models/props/reflection_cube.mdl", 0),
        (4, "models/props_gameplay/mp_ball.mdl", 0),
        (5, "models/props_underground/underground_weighted_cube.mdl", 0),
    ];
    for (skin, model, recomputed) in cases {
        let mut server = Server::new();
        server.level_init("test", &cube_map(&[("skin", &skin.to_string())]), &[]);
        let entity = find_named(&server, "cube");
        assert_eq!(
            entity.core.model.as_deref(),
            Some(model),
            "skin {skin} chose the wrong model"
        );
        // …and the key it came from is *gone*: the skin is recomputed from the
        // type, so a cube that shipped with `skin 3` draws with skin 0.
        assert_eq!(
            cube(&server, "cube").skin,
            recomputed,
            "skin {skin} was not recomputed"
        );
    }
}

/// `NewSkins 1` takes `CubeType` instead, and then `skin` really is ignored.
#[test]
fn new_skins_takes_the_cube_type_key_and_leaves_the_skin_key_alone() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &cube_map(&[("NewSkins", "1"), ("CubeType", "3"), ("skin", "5")]),
        &[],
    );
    // `CubeType 3` is a sphere. The `skin 5` would have been an antique cube
    // had `ConvertOldSkins` run, so this distinguishes the two paths.
    assert_eq!(
        find_named(&server, "cube").core.model.as_deref(),
        Some("models/props_gameplay/mp_ball.mdl")
    );
    assert_eq!(cube(&server, "cube").cube_type, classes::prop::CubeType::Sphere);
}

/// The skin ladder, on the four combinations a shipped map can actually
/// produce — `SetCubeSkin`'s `default` arms, because no shipped cube is
/// bounce- or speed-painted and nothing here can set `m_bActivated`.
#[test]
fn the_skin_comes_from_the_type_and_the_rust_flag() {
    // (CubeType, SkinType, expected skin)
    let cases = [
        (0, 0, 0),
        // "Rusted cubes don't show paint" — the standard cube is the only one
        // that reads the flag outside the paint switch.
        (0, 1, 3),
        (1, 0, 1),
        // The companion cube has no rusted skin at all: the flag is not read
        // on its arm, so it stays 1.
        (1, 1, 1),
        (2, 0, 0),
        // …but the reflective cube does read it, in its unpainted arm.
        (2, 1, 1),
        (3, 0, 0),
        (4, 0, 0),
    ];
    for (cube_type, skin_type, expected) in cases {
        let mut server = Server::new();
        server.level_init(
            "test",
            &cube_map(&[
                ("NewSkins", "1"),
                ("CubeType", &cube_type.to_string()),
                ("SkinType", &skin_type.to_string()),
            ]),
            &[],
        );
        assert_eq!(
            cube(&server, "cube").skin,
            expected,
            "CubeType {cube_type} SkinType {skin_type}"
        );
    }
}

/// `SetPaint` repaints, and the skin follows — the paint arms of the ladder,
/// which no shipped map reaches but the input does.
#[test]
fn painting_a_cube_changes_its_skin_and_fires_onpainted_once() {
    let map = vec![
        block(&[
            ("classname", "prop_weighted_cube"),
            ("targetname", "cube"),
            ("origin", "0 0 0"),
            ("NewSkins", "1"),
            ("CubeType", "0"),
            ("OnPainted", &conn("painted", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "painted"),
            ("max", "10"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    let id = find_named(&server, "cube").id();
    assert_eq!(cube(&server, "cube").skin, 0);
    assert_eq!(counter_value(&server, "painted"), 0.0, "not painted yet");

    // `BOUNCE_POWER` — `CUBE_STANDARD_BOUNCE_SKIN`.
    server.accept_input(id, "SetPaint", Variant::Int(0), None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(cube(&server, "cube").skin, 6);
    assert_eq!(counter_value(&server, "painted"), 1.0);

    // **Painting it the colour it already is fires nothing** — the guard at
    // the top of `SetPaintedMaterial` compares before it assigns.
    server.accept_input(id, "SetPaint", Variant::Int(0), None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(counter_value(&server, "painted"), 1.0, "a repeat is not a change");

    // `SPEED_POWER`, which is a change and does fire.
    server.accept_input(id, "SetPaint", Variant::Int(2), None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(cube(&server, "cube").skin, 7);
    assert_eq!(counter_value(&server, "painted"), 2.0);

    // …and `NO_POWER` never fires, whatever it is changing from.
    server.accept_input(id, "SetPaint", Variant::Int(4), None, None, 0);
    run(&mut server, 0.02);
    assert_eq!(cube(&server, "cube").skin, 0, "back to the clean skin");
    assert_eq!(counter_value(&server, "painted"), 2.0, "NO_POWER is not a paint");
}

/// **A cube that ships pre-painted fires `OnPainted` on the first tick**, from
/// an entity nothing has touched.
///
/// That is `CPropWeightedCube::Activate` calling `SetPaintedMaterial` and
/// *then* `BaseClass::Activate`, which paints again — the first call is the
/// change and fires, the second is a repeat and does not. 23 of the game's 98
/// cubes ship with `PaintPower 3` and reach exactly this.
#[test]
fn a_cube_with_a_prepainted_power_fires_onpainted_when_the_map_starts() {
    let map = vec![
        block(&[
            ("classname", "prop_weighted_cube"),
            ("targetname", "cube"),
            ("origin", "0 0 0"),
            // `PORTAL_POWER`, which is what those 23 write.
            ("PaintPower", "3"),
            ("OnPainted", &conn("painted", "Add", "1", "0", "-1")),
        ]),
        block(&[
            ("classname", "math_counter"),
            ("targetname", "painted"),
            ("max", "10"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    run(&mut server, 0.05);
    assert_eq!(counter_value(&server, "painted"), 1.0, "once, not twice");
    // `PORTAL_POWER` takes `SetCubeSkin`'s `default` arm, so the skin is the
    // clean one despite the cube being painted — which is why all 98 shipped
    // cubes end up on one of four skins.
    assert_eq!(cube(&server, "cube").skin, 0);
    assert_eq!(
        cube(&server, "cube").painted_power,
        classes::prop::PaintPower::Portal
    );
}

/// Both dissolve inputs fire `OnFizzled` and remove the cube.
///
/// 63 shipped connections listen to `OnFizzled`, nearly all of them a dropper
/// being told to make another cube — so this is the one thing a map does with
/// a cube that this port can carry through end to end.
#[test]
fn dissolving_a_cube_fires_onfizzled_and_removes_it() {
    for input in ["Dissolve", "SilentDissolve"] {
        let map = vec![
            block(&[
                ("classname", "prop_weighted_cube"),
                ("targetname", "cube"),
                ("origin", "0 0 0"),
                ("OnFizzled", &conn("fizzled", "Add", "1", "0", "-1")),
            ]),
            block(&[
                ("classname", "math_counter"),
                ("targetname", "fizzled"),
                ("max", "10"),
            ]),
        ];
        let mut server = Server::new();
        server.level_init("test", &map, &[]);
        let id = find_named(&server, "cube").id();

        server.accept_input(id, input, Variant::Void, None, None, 0);
        run(&mut server, 0.05);
        assert_eq!(counter_value(&server, "fizzled"), 1.0, "{input}");
        assert!(
            server.entities.get(id).is_none(),
            "{input} left the cube alive"
        );
    }
}

/// The four flags that are parsed, kept and toggled but that nothing in this
/// port reads — recorded as behaviour so that the day something does read one,
/// the plumbing is already known to work.
#[test]
fn the_funnel_and_pickup_flags_are_kept_and_toggled() {
    let mut server = Server::new();
    server.level_init("test", &cube_map(&[("allowfunnel", "0")]), &[]);
    let id = find_named(&server, "cube").id();
    assert!(!cube(&server, "cube").allow_portal_funnel, "the key is read");
    assert!(!cube(&server, "cube").pickup_disabled, "Spawn clears it");

    server.accept_input(id, "EnablePortalFunnel", Variant::Void, None, None, 0);
    server.accept_input(id, "DisablePickup", Variant::Void, None, None, 0);
    run(&mut server, 0.02);
    assert!(cube(&server, "cube").allow_portal_funnel);
    assert!(cube(&server, "cube").pickup_disabled);

    server.accept_input(id, "DisablePortalFunnel", Variant::Void, None, None, 0);
    server.accept_input(id, "EnablePickup", Variant::Void, None, None, 0);
    run(&mut server, 0.02);
    assert!(!cube(&server, "cube").allow_portal_funnel);
    assert!(!cube(&server, "cube").pickup_disabled);
}

/// `SetCubeType`'s opening line makes the Schrodinger cube a reflective one,
/// under a `FIXME` that was never acted on — so the twin-linking arm below it
/// is dead code in the shipped game, and this is what says so.
#[test]
fn a_schrodinger_cube_is_remapped_to_a_reflective_one() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &cube_map(&[("NewSkins", "1"), ("CubeType", "5")]),
        &[],
    );
    assert_eq!(
        cube(&server, "cube").cube_type,
        classes::prop::CubeType::Reflective
    );
    assert_eq!(
        find_named(&server, "cube").core.model.as_deref(),
        Some("models/props/reflection_cube.mdl")
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
    // Written onto 599 `prop_dynamic`s by Hammer and declared by no `.fgd` in
    // the depot; nothing in `legacy/` reads either name. Mapper leftovers, the
    // same category as `inputfilter`.
    ("canbecaptured", 599),
    ("detailvbsp", 106),
    // `disableX360` **is** in `base.fgd`'s `SystemLevelChoice`, beside the four
    // CPU/GPU-level keys the port now consumes — and unlike those four it is
    // read by nothing in the whole tree, on either side of the DLL boundary.
    ("disablex360", 123),
    ("filtername", 1),
    ("inputfilter", 2497),
    ("mapversion", 106),
    // The DirectX-level fade pair. In no Portal 2 `.fgd` and read nowhere in
    // `legacy/`: they are HL2-era Hammer keys that survived in copied prefabs.
    ("maxdxlevel", 3924),
    ("message", 3),
    ("mindxlevel", 3924),
    ("npcpoints", 1),
    ("onendtouchblueplayer", 1),
    ("onendtouchorangeplayer", 1),
    // One `prop_dynamic` declares an output only a `func_portal_cleanser` has;
    // ten declare `func_door`'s. Mapper mistakes, now visible because the class
    // that carries them is implemented.
    ("onfizzled", 1),
    // One `func_areaportal` in the game connects an `OnFullyClosed`, which
    // `CAreaPortal` does not declare either — a `func_door`'s output left on a
    // copy-pasted entity.
    ("onfullyclosed", 1),
    ("onfullyopen", 10),
    ("onproxyrelay", 135),
    ("onstarttouchblueplayer", 1),
    ("onstarttouchorangeplayer", 1),
    ("ontrigger", 15),
    ("onunpressed", 2),
    ("paintinmap", 25),
    ("scalevalue", 599),
    ("skin", 1),
    ("sunspreadangle", 27),
    // VScript's two keys, and the pair rises together: 33 chamber doors carry
    // a `vscripts` and two a `thinkfunction`, which are the puzzle-completion
    // and door-close-sensor Squirrel files. There is no VScript here.
    ("thinkfunction", 2),
    ("vrad_brush_cast_shadows", 2456),
    ("vscripts", 72),
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
    let mut cube_skins: BTreeMap<(String, i32), usize> = BTreeMap::new();
    let mut named_lights = 0;
    let mut maps_with_a_master = 0;
    let mut custom_max = 0;
    let mut peak_thinks = 0;
    // Stage 3's metric: brush entities, and how many of them are somewhere
    // other than where the entity lump put them once the map has run.
    let mut brush_entities = 0;
    let mut moved = 0;
    let mut still_moving = 0;
    // Of the brush entities that moved, the ones that have a parent and no
    // motion of their own — carried rather than driven.
    let mut carried = 0;
    let mut carried_furthest = 0.0f32;
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
        // …and every entity alive is one of those, or one another entity's
        // `Spawn` made — a `prop_floor_button`'s trigger.
        assert_eq!(
            stats.spawned + stats.removed_on_spawn,
            stats.matched + stats.created,
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
            // What every shipped cube ends up wearing, after `ConvertOldSkins`
            // has turned its `skin` key into a type and `SetCubeSkin` has
            // computed a fresh skin from that. See `WeightedCube`.
            if let Some(cube) = entity
                .behaviour
                .downcast_ref::<classes::prop::WeightedCube>()
            {
                *cube_skins
                    .entry((
                        entity.core.model.clone().unwrap_or_default(),
                        cube.skin,
                    ))
                    .or_default() += 1;
            }
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
        total.created += stats.created;
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

        // **The transform pair's invariant, on every entity of every map.**
        // The world pair is derived state — `hierarchy` recomputes it from the
        // local pair whenever anything moves — so a direct write to `origin`
        // that skipped `set_abs_placement` would leave the two disagreeing and
        // the next time the parent moved, the entity would snap. Checking it
        // here is what makes that a test failure rather than a bug report
        // about a door that teleports.
        for (id, entity) in server.entities.iter() {
            let Some(parent) = entity.parent() else {
                assert_eq!(
                    (entity.local_origin, entity.local_angles),
                    (entity.origin, entity.angles),
                    "{name}: unparented {} [{}] has two different placements",
                    entity.classname(),
                    id.slot()
                );
                continue;
            };
            let Some(parent) = server.entities.get(parent) else {
                continue;
            };
            assert!(
                parent.children().contains(&id),
                "{name}: {} [{}] names a parent that does not name it back",
                entity.classname(),
                id.slot()
            );
            let expected = parent.to_world()
                * glam::Affine3A::from_mat3_translation(
                    crate::math::angle_matrix(entity.local_angles),
                    entity.local_origin,
                );
            let expected = glam::Vec3::from(expected.translation);
            assert!(
                (entity.origin - expected).length() < 0.01,
                "{name}: {} [{}] is at {:?} but its parent ({}) puts it at {expected:?}",
                entity.classname(),
                id.slot(),
                entity.origin,
                parent.classname(),
            );
        }

        for (index, origin, angles) in placed {
            let Some(entity) = server.brush_entity(index) else {
                continue;
            };
            if entity.origin != origin || entity.angles != angles {
                moved += 1;
                if entity.parent().is_some() && entity.velocity == glam::Vec3::ZERO {
                    carried += 1;
                    carried_furthest = carried_furthest.max((entity.origin - origin).length());
                }
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
    println!(
        "    {carried} of the movers' children were carried rather than driven, \
         the furthest by {carried_furthest:.1} units"
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
    assert_eq!(
        total.spawned + total.removed_on_spawn,
        total.matched + total.created
    );
    assert_eq!(total.removed_on_spawn, 6_937, "unnamed lights");

    // The parse side. Stage 1 matched 17,069 blocks and spawned 10,132,
    // stage 2 took it to 19,229 and 12,292, stage 3's six brush classes were
    // 3,410 more of both, stage 4's twelve — five triggers, six filters and
    // `point_teleport` — are 3,322 more again, `prop_floor_button` is 65,
    // stage 5's two — `logic_playerproxy` and `player_loadsaved` — are 9 each,
    // and **`prop_dynamic` is 8,462 on its own**, which is more than stages 3
    // and 4 together. `prop_testchamber_door` is 138 after it,
    // `logic_branch_listener` — the class that shuts those doors — is 158, and
    // `prop_portal` is 21.
    //
    // **+409 for the two areaportal classnames** — 206 `func_areaportal` and
    // 203 `func_areaportalwindow` — which arrived with `world/`'s visibility
    // and are the first entities here whose whole effect is on what the
    // renderer draws. Both spawn, because neither removes itself.
    //
    // `func_areaportalwindow`'s two fade inputs stay **unhandled** (43
    // `SetFadeStartDistance` and 43 `SetFadeEndDistance`) rather than being
    // declared and ignored: the distance fade is not ported, and an input
    // that is accepted and does nothing is worse than one that is reported.
    //
    // **+98 for `prop_weighted_cube`**, across 59 maps — the first
    // `CPhysicsProp` here, and one whose simulation is entirely absent while
    // its map-facing half is not.
    assert_eq!(total.matched, 35_330);
    assert_eq!(total.spawned, 28_458);
    // +593 over stage 5, and 326 of them are `OnUser1`: a `prop_dynamic`'s
    // connections used to be keys on a block with no class. The other 267 are
    // `OnAnimationDone` (181), `OnBreak` (16), `OnAnimationBegun` (15) and
    // `OnUser2`-`OnUser4`.
    // +247 for the doors: `OnFullyClosed` (150), `OnOpen` (66) and
    // `OnFullyOpen` (31). **No shipped map connects an `OnClose`.**
    // +307 for the branch listeners, and they are as lopsided: `OnAllTrue`
    // (272), `OnAllFalse` (19), `OnMixed` (16).
    // **+1 for the portals**, and that is the whole of `prop_portal`'s output
    // surface in the shipped game: `sp_a1_intro1`'s
    // `portal_red_0.OnPlayerTeleportFromMe`. The other four outputs the class
    // declares are connected by no map.
    // **+96 for the cubes**: 63 `OnFizzled`, 22 `OnUser1`, 7 `OnPlayerPickup`
    // and one each of `OnBluePickUp`, `OnOrangePickUp`, `OnPainted` and
    // `OnPhysGunDrop`. Four of those names are pickup outputs the port cannot
    // fire; they are declared so the connection parses as an output rather
    // than as an unknown key. See `WEIGHTED_CUBE_OUTPUTS`.
    assert_eq!(total.outputs, 54_631);
    // **-1 classname and -21 occurrences**, both `prop_portal`: it was the
    // only one of the five names the class table gained that any map places.
    // **-2 and -409 again** for the two areaportal classnames, both of which
    // every map that has them places.
    // **-1 classname and -98 occurrences** for `prop_weighted_cube`, which
    // every map that has cubes places.
    assert_eq!(total.unknown.len(), 156);
    assert_eq!(total.unknown.values().sum::<usize>(), 25_595);
    // **The first entities in this port that are not in a `.bsp`.** One
    // `trigger_portal_button` per `prop_floor_button`, made by its `Spawn`
    // through `Context::create_entity` — so `spawned` is 130 larger than the
    // stage before rather than 65.
    assert_eq!(total.created, 65, "trigger_portal_button, one per button");
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
    assert_eq!(per_class.get("logic_branch_listener"), Some(&158));
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
    // Stage 5's. Nine of each, and **all five of the game's
    // `logic_playerproxy` output connections are on `sp_a1_intro1`** — the map
    // this port loads by default.
    assert_eq!(per_class.get("logic_playerproxy"), Some(&9));
    assert_eq!(per_class.get("player_loadsaved"), Some(&9));
    assert_eq!(per_class.get("filter_activator_model"), Some(&1));
    // Portal 2's own, and the pair that proves runtime entity creation works
    // against real map data: 65 buttons in 47 of the 106 maps, and 65
    // triggers that appear in no entity lump at all.
    assert_eq!(per_class.get("prop_floor_button"), Some(&65));
    // …and `prop_dynamic`, which is the commonest thing in a Portal 2 map
    // after `logic_relay` — by ten entities. The two classnames a map can
    // place are one C++ class and two different behaviours; see
    // `DynamicProp::is_plain_dynamic`.
    assert_eq!(per_class.get("prop_dynamic"), Some(&8_072));
    assert_eq!(per_class.get("prop_dynamic_override"), Some(&390));
    assert_eq!(
        per_class.get("dynamic_prop"),
        None,
        "registered, never placed"
    );
    assert_eq!(per_class.get("prop_dynamic_glow"), None);
    assert_eq!(per_class.get("trigger_portal_button"), Some(&65));
    // 98 across 59 maps, one of them on `sp_a1_intro1`. Every one of them is
    // drawn, and none of them falls.
    assert_eq!(per_class.get("prop_weighted_cube"), Some(&98));
    println!("  what each shipped cube ends up wearing:");
    for ((model, skin), count) in &cube_skins {
        println!("    {count:>4}  {model} skin {skin}");
    }
    // **The whole of what the shipped maps can produce**, and the measurement
    // behind two claims. First, that `ConvertOldSkins` is doing its job: the
    // four distinct models are the four cube types the maps actually place,
    // and 77 of the 98 got there from a `skin` key rather than a `CubeType`.
    // Second, the size of the skin-family gap — **15 of the 98 end on a
    // non-zero skin** (5 companion, 8 rusted standard, 2 rusted reflective)
    // and the renderer draws skin 0 for all of them, because `.mdl`'s skin
    // table is not read yet. One of the 15 is on `sp_a1_intro1`.
    assert_eq!(
        cube_skins
            .iter()
            .map(|((m, s), n)| (m.as_str(), *s, *n))
            .collect::<Vec<_>>(),
        vec![
            ("models/props/metal_box.mdl", 0, 31),
            ("models/props/metal_box.mdl", 1, 5),
            ("models/props/metal_box.mdl", 3, 8),
            ("models/props/reflection_cube.mdl", 0, 24),
            ("models/props/reflection_cube.mdl", 1, 2),
            ("models/props_gameplay/mp_ball.mdl", 0, 14),
            ("models/props_underground/underground_weighted_cube.mdl", 0, 14),
        ],
        "what the shipped cubes wear has changed"
    );
    assert_eq!(
        cube_skins
            .iter()
            .filter(|((_, skin), _)| *skin != 0)
            .map(|(_, n)| n)
            .sum::<usize>(),
        15,
        "cubes whose skin the renderer cannot yet draw"
    );
    // `portdocs/PORTAL.md` stage 2. 21 across 10 maps, two of them on
    // `sp_a1_intro1` — the map this port loads by default, which makes this
    // the rare class whose test bed is already on screen.
    assert_eq!(per_class.get("prop_portal"), Some(&21));
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
    //
    // **`accepted` and `thinks` both jumped with `prop_dynamic`** — 1,443 more
    // inputs land and 2,970 more thinks run, because a prop that is given an
    // animation wakes at 10 Hz until it has finished one. `no_target` fell by
    // more than half for the same reason: most events used to reach nothing
    // because most *targets* were props.
    assert_eq!(io.dispatched, 5_787);
    // **+2 with `prop_portal`, and `no_target` falls by the same 2**: the
    // `SetActivatedState` a map used to aim at a classname nothing answered
    // for now lands. Only two of the game's 31 are fired inside two seconds.
    // **+31 with the parenting pair.** `SetParent` and `ClearParent` are base
    // inputs now that `EntityCore` carries a local/abs transform pair, and 31
    // of the game's 143 `SetParent` connections fire inside the first two
    // seconds — 16 `prop_dynamic_override`, 12 `func_brush`, 2 `prop_dynamic`
    // and 1 `info_target`. No `ClearParent` does.
    //
    // **+1,074 with `LookupAttachment`.** Both `SetParentAttachment*` forms
    // are base inputs now, and they are accepted even here, where the maps
    // carry no models at all: Valve's two guards `return` rather than falling
    // back, so a declared input that ran and declined is *handled*. This test
    // therefore measures that they are reached; what they do with a model in
    // hand is `every_shipped_attachment_connection_puts_its_entity_on_a_bone`.
    //
    // **+5 with `prop_weighted_cube`**, and that is the whole of what the
    // shipped maps fire at a cube inside two seconds. A cube is scenery that
    // is acted on late — its 63 `OnFizzled` connections are *outputs*, and
    // what fires its inputs is a dropper or a fizzler the player has to reach.
    assert_eq!(io.accepted, 5_042);
    // **+2,898, and every one of them is a chamber door.** `AnimateThink`
    // re-arms unconditionally, which is Valve's, so all 138 doors wake ten
    // times a second for the whole level — 2 seconds at a `SetNextThink`
    // quantised to six 64 Hz ticks. See `TestChamberDoor::animate_think` for
    // why this class deliberately does not take `DynamicProp`'s
    // cancel-when-idle divergence.
    assert_eq!(io.thinks, 7_318);
    // **-86 with the two areaportal classnames, and `accepted` does not
    // move** — every one of the 86 is refused rather than accepted. They are
    // `func_areaportalwindow`'s two fade inputs, fired at the *classname*
    // rather than at a name, so before the class existed they found no target
    // at all; now they find 203 windows that do not implement the distance
    // fade. The 86 moved from `no_target` to `unhandled`, which is the list
    // below.
    //
    // **-11 with `prop_weighted_cube`**: six more events than the five
    // accepted above now find a cube, and are refused by it rather than
    // finding nothing — they are on the unhandled list below.
    assert_eq!(io.no_target, 1_025);

    // Nothing may fail to convert: every shipped connection's parameter is
    // compatible with the input it is aimed at.
    assert_eq!(io.bad_conversion, 0, "a shipped map has a bad I/O link");

    // The whole set of inputs that reach an implemented class and are refused.
    //
    // **The parenting family is no longer on this list at all.** The transform
    // pair took `SetParent` and `ClearParent` off it, and `LookupAttachment`
    // took both `SetParentAttachment*` forms — which between them were 1,072
    // of the occurrences here. They are *accepted* now even on this test's
    // maps, which carry no models at all, because `SetParentAttachment`'s two
    // guards `return` rather than falling back: a declared input that ran and
    // declined is handled, not unhandled. What it does when the models *are*
    // in hand is
    // `every_shipped_attachment_connection_puts_its_entity_on_a_bone`.
    //
    // Three of the remaining names are the player procedurals (stage 5's), one
    // is `RunScriptCode` (`portdocs/SERVER.md` §9), and
    // `prop_dynamic.Disabled` is eight connections misspelling `Disable`.
    //
    // **`prop_weighted_cube` added two names and six occurrences, and both
    // belong here rather than in the class.** `EnableMotion` is
    // `CPhysicsProp`'s and there is no motion to enable; `AddOutput` is
    // `CBaseEntity`'s and no class in the port implements it. Declaring either
    // would make it accepted and inert, which this list exists to avoid.
    let unhandled: Vec<(&str, usize)> =
        io.unhandled.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        unhandled,
        vec![
            ("!player_blue (no such player)", 37),
            ("!player_orange (no such player)", 37),
            // The distance fade `CFuncAreaPortalWindow` does by itself, which
            // needs a per-view update inside the render loop and the fogged
            // pane to stand in for what is behind it. Neither exists, so the
            // window is an areaportal that is simply open — it draws through
            // one the shipped game would have shut, which is too much rather
            // than too little. See `classes::AreaPortal`.
            ("func_areaportalwindow.SetFadeEndDistance", 43),
            ("func_areaportalwindow.SetFadeStartDistance", 43),
            ("logic_relay.RunScriptCode", 1),
            ("player.SetFogController", 97),
            ("prop_dynamic.Disabled", 8),
            ("prop_weighted_cube.AddOutput", 5),
            ("prop_weighted_cube.Enablemotion", 1),
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
    //
    // **Parenting took it from 67 to 106, and the 39 are the whole reason the
    // transform pair exists**: they are brush entities with no velocity of
    // their own that a parent dragged somewhere. `carried` counts them
    // directly — 52, including the ones that also moved by themselves — and
    // the furthest ride in the game's first two seconds is **539 units**, a
    // clip brush on the lift that opens a chamber. Before the pair, all 52 sat
    // where the lump put them while the thing they are bolted to drove off.
    assert_eq!(brush_entities, 6_302);
    assert_eq!(moved, 106, "brush entities that left their spawn placement");
    assert_eq!(carried, 52, "…of which this many were carried by a parent");
    assert!(
        carried_furthest > 539.0 && carried_furthest < 540.0,
        "the longest carried ride moved: {carried_furthest}"
    );
    assert_eq!(still_moving, 34, "…and were still travelling at 2s");

    // Stage 4's own parse-side number, and it is no longer only about brush
    // entities: how many entities are *live* triggers two ticks into the map —
    // `FSOLID_TRIGGER` set, so the touch pass will look at them. 2,892 brush
    // triggers are placed and 637 of them are `StartDisabled`, including
    // **107 of the game's 110 `trigger_teleport`s**, leaving 2,255; the other
    // **65 are `SOLID_OBB` and have no brush model at all**, one over each
    // `prop_floor_button`.
    //
    // `every_shipped_maps_triggers_notice_the_player` walks a player into each
    // of the 2,255 and `every_shipped_floor_button_presses` stands one on each
    // of the 65 — the two halves of the same claim, split because they are
    // answered by different code.
    //
    // **`prop_portal` takes it to 2,341**, and those 21 are the third kind:
    // `SOLID_OBB` like a floor button's trigger, but the entity the map placed
    // rather than one made at spawn. They are live whether or not the portal
    // is `Activated` — `CPortal_Base2D::Spawn` sets `FSOLID_TRIGGER`
    // unconditionally — which is what stage 4's teleport will hang off.
    assert_eq!(triggers, 2_341);

    // `ThinkList` is a flat `Vec` with a linear scan, which is only the right
    // shape while this number is small. It is the measurement `think.rs` cites.
    //
    // **Stage 3 put every moving entity in this list and the peak did not
    // change**, which is the measurement `think.rs` said to retake: a mover is
    // only in it while it is actually travelling, and the 43 is set by the
    // `logic_auto` bootstrap rather than by anything that moves. Stage 4 takes
    // it to 48 — a `trigger_multiple` holds a think for its whole `wait`, and
    // a `trigger_once` for the tenth of a second before it deletes itself.
    //
    // **`prop_dynamic` takes it to 214, and that is the first time this number
    // has said anything about the shape of the list.** A prop that is given an
    // animation thinks at 10 Hz until that animation *ends*, and a map's
    // bootstrap starts a lot of them at once. It is still a list being entered
    // and left rather than filled once — `AnimThink` cancels itself the moment
    // its sequence cannot end (`rustdocs/SERVER.md` gotcha 64), so a prop that
    // is looping or holding is **not** in here — and 215 against 27,930 live
    // entities is still under one per cent. The 215th is a chamber door,
    // which never leaves the list at all.
    assert_eq!(peak_thinks, 215);

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
    /// A fixture of hand-placed boxes has no world, so nothing can be inside
    /// one.
    fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
        false
    }

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
        move_type: crate::server::movement::MoveType::Walk,
        health: 100,
        life_state: Default::default(),
        flags: 0,
        buttons: 0,
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
    let state = player_at(Vec3::ZERO);
    server.spawn_player(state);
    server.set_player_state(state);
    // **`noclip` is the server's since stage 5**, so this is the command and
    // not a field on the state that arrives: `set_player_state` deliberately
    // ignores the move type it is handed.
    assert_eq!(server.toggle_noclip(), Some(true));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 0.1);
    close(
        server.player_state().expect("a player").base_velocity,
        Vec3::ZERO,
    );
}

/// `trigger_hurt` fires its outputs on Valve's half-second cadence — and
/// since `portdocs/SERVER.md` stage 5 it also takes health away, `m_flDamage`
/// per **second** rather than per dose.
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
    // `damage 20` is 20 a second, dealt every half second, so each of the
    // three doses is **10** and not 20. Getting that wrong doubles the
    // lethality of every `trigger_hurt` in the game.
    assert_eq!(player_health(&server), 70);

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
            sequences,
            attachments,
            ..
        } = &mut server;
        let mut cx = Context::new(
            time,
            queue,
            random,
            entities,
            Some(player),
            sequences,
            attachments.as_ref(),
        );
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

/// The engine's half of the touch query, over a map's placed brush models.
///
/// `engine/mod.rs`'s `WorldTouchQuery` without a `World` around it — a depot
/// test has no GPU and so cannot build one — and **the same function body**,
/// which is why `world/` exposes it free. Shared by the two depot tests that
/// need a player to touch things.
struct Placed<'a> {
    collision: &'a crate::engine::trace::CollisionBsp,
    models: &'a [crate::engine::world::PlacedBrushModel],
    /// [`push_trace`](TouchQuery::push_trace)'s scratch clip chain, exactly as
    /// `WorldTouchQuery` holds one.
    chain: Vec<crate::engine::trace::BrushModel>,
}

impl TouchQuery for Placed<'_> {
    fn push_trace(
        &mut self,
        clip: crate::server::PushClip,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pushers: &[crate::server::Pusher],
    ) -> crate::server::PushHit {
        crate::engine::push_trace(
            self.collision,
            self.models,
            &mut self.chain,
            clip,
            start,
            end,
            mins,
            maxs,
            pushers,
        )
    }

    fn start_solid(&mut self, origin: Vec3, mins: Vec3, maxs: Vec3) -> bool {
        let ray = crate::engine::trace::Ray::hull(origin, origin, mins, maxs);
        self.collision
            .tracer()
            .trace(&ray, crate::engine::trace::Contents::MASK_PLAYERSOLID)
            .start_solid
    }

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
    // Probe points that are *also* on a `prop_floor_button`, which is the only
    // reason a brush trigger's probe can dispatch something a brush trigger
    // did not cause.
    let mut also_on_a_button = 0usize;
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
                chain: Vec::new(),
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
            // A `SOLID_OBB` trigger the same hull is standing in — see
            // `also_on_a_button`.
            if server.entities.iter().any(|(_, e)| {
                e.classname() == "trigger_portal_button" && !e.core.touch_links.is_empty()
            }) {
                also_on_a_button += 1;
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
         {withdrawn} were switched off or deleted by the map before the second tick;\n  \
         {also_on_a_button} of the probe points are also on a prop_floor_button"
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
    // 1,889 of them get as far as dispatching something, which is the whole
    // chain — geometry, `FSOLID_TRIGGER`, the touch link, `PassesTriggerFilters`
    // and an output with a connection on it. The 357 that do not are triggers
    // whose outputs go to entities this port has no class for, or whose filter
    // says "cubes only". It was 1,888 before `prop_dynamic`: one more trigger
    // in the game has a connection whose only target is a prop, and the
    // number rises again with every class that lands.
    //
    // > **9 of those 1,888 are not the brush trigger's doing, and the number
    // > is the difference between two measurements rather than a guess.**
    // > A probe point is a place a player fits, and in **21** of them a
    // > `prop_floor_button` sits inside the same brush trigger — so standing
    // > there presses the pad as well and the press dispatches events of its
    // > own. Twelve of those 21 belong to triggers that were already firing
    // > something, so the count moved by nine. That is the shipped game's
    // > behaviour — a chamber's exit trigger around its own button is ordinary
    // > level design — and it is counted rather than filtered out so that the
    // > number is explained rather than absorbed.
    assert_eq!(fired, 1_889);
    assert_eq!(also_on_a_button, 21, "probes that also stand on a pad");
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
        // A `SOLID_OBB` trigger: the same triple, with a box in place of the
        // brush model — so the shape is answered inside this module rather
        // than by the engine. Spawned straight from a lump here, which no map
        // does; a real one is made by the button below.
        ("trigger_portal_button", &[], true, false),
        // …and the button itself is `SOLID_VPHYSICS` and genuinely solid,
        // which nothing can yet collide with: its collision is a `.phy`.
        ("prop_floor_button", &[], true, true),
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

// ===========================================================================
// prop_floor_button — the pad you stand on
// ===========================================================================

/// A map with one `prop_floor_button` named `pad`, wired to two counters.
///
/// `extra` goes on the button, which is how these tests place and turn it.
fn button_map(extra: &[(&str, &str)]) -> Vec<bsp::Entity> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("classname", "prop_floor_button"),
        ("targetname", "pad"),
        ("model", "models/props/portal_button.mdl"),
    ];
    pairs.extend_from_slice(extra);
    let mut button = block(&pairs);
    button
        .pairs
        .push(("OnPressed".to_owned(), conn("down", "Add", "1", "0", "-1")));
    button
        .pairs
        .push(("OnUnPressed".to_owned(), conn("up", "Add", "1", "0", "-1")));

    vec![
        block(&[("classname", "worldspawn")]),
        button,
        block(&[("classname", "math_counter"), ("targetname", "down")]),
        block(&[("classname", "math_counter"), ("targetname", "up")]),
    ]
}

/// The button's own state, through the handle `ent_dump` would use.
fn pad(server: &Server) -> &classes::FloorButton {
    find_named(server, "pad")
        .behaviour
        .downcast_ref::<classes::FloorButton>()
        .expect("a FloorButton")
}

/// The `trigger_portal_button` a button made, which has no `targetname` and so
/// has to be found by class.
fn pad_trigger(server: &Server) -> &Entity {
    server
        .entities
        .iter()
        .find(|(_, e)| e.classname() == "trigger_portal_button")
        .map(|(_, e)| e)
        .expect("the button created its trigger")
}

/// **The whole class in one test**: `Spawn` makes a second entity, and that
/// entity is a box trigger sitting exactly where the pad is.
///
/// This is `Context::create_entity`'s first consumer, so it is also the test
/// that says a runtime-created entity gets spawned at all.
#[test]
fn a_floor_button_creates_its_own_trigger() {
    let mut server = Server::new();
    server.level_init("test", &button_map(&[("origin", "100 200 64")]), &[]);

    let trigger = pad_trigger(&server);
    assert_eq!(trigger.origin, Vec3::new(100.0, 200.0, 64.0));
    // `UTIL_SetSize( pTrigger, (-20,-20,0), (20,20,14) )`.
    assert_eq!(trigger.model_bounds.mins, Vec3::new(-20.0, -20.0, 0.0));
    assert_eq!(trigger.model_bounds.maxs, Vec3::new(20.0, 20.0, 14.0));

    // The triple that makes it a trigger rather than a wall, and the `Obb`
    // that decides *which module* answers for its shape.
    assert_eq!(trigger.solid, crate::server::movement::Solid::Obb);
    assert!(trigger.is_solid_flag_set(crate::server::movement::FSOLID_TRIGGER));
    assert!(!trigger.is_solid(), "you walk through a button's trigger");

    // And the button knows it is up.
    assert!(!pad(&server).pressed);
}

/// Standing on the pad presses it, and stepping off releases it.
#[test]
fn standing_on_a_floor_button_presses_it_and_stepping_off_releases_it() {
    let mut server = Server::new();
    server.level_init("test", &button_map(&[("origin", "0 0 0")]), &[]);
    server.spawn_player(player_at(Vec3::new(500.0, 0.0, 0.0)));

    run_touching(&mut server, &mut NoTouchQuery, 0.2);
    assert!(!pad(&server).pressed);
    assert_eq!(counter_value(&server, "down"), 0.0);

    // On it.
    server.set_player_state(player_at(Vec3::ZERO));
    run_touching(&mut server, &mut NoTouchQuery, 0.1);
    assert!(pad(&server).pressed);
    assert_eq!(counter_value(&server, "down"), 1.0);
    assert_eq!(counter_value(&server, "up"), 0.0);

    // Still on it: `OnStartTouchAll` does not re-fire.
    run_touching(&mut server, &mut NoTouchQuery, 0.5);
    assert_eq!(counter_value(&server, "down"), 1.0);

    // Off it. The pad is 40 across and the hull 32, so 40 units clears it.
    server.set_player_state(player_at(Vec3::new(40.0, 0.0, 0.0)));
    run_touching(&mut server, &mut NoTouchQuery, 0.1);
    assert!(!pad(&server).pressed);
    assert_eq!(counter_value(&server, "up"), 1.0);
    assert_eq!(counter_value(&server, "down"), 1.0);
}

/// The one divergence in the class: the trigger sends its owner a `PressIn`
/// rather than calling into it, which costs **one extra event and no tick**.
///
/// The queue restarts from the head after every event (gotcha 4), so the whole
/// chain — trigger → `pad.PressIn` → `down.Add` — lands inside the single tick
/// the touch happened in. If that were ever to become two ticks, every floor
/// button in the game would answer a frame late.
#[test]
fn a_press_completes_in_the_tick_it_started_in() {
    let mut server = Server::new();
    server.level_init("test", &button_map(&[("origin", "0 0 0")]), &[]);
    server.spawn_player(player_at(Vec3::new(500.0, 0.0, 0.0)));
    run_touching(&mut server, &mut NoTouchQuery, 0.2);

    server.set_player_state(player_at(Vec3::ZERO));
    ticks_touching(&mut server, &mut NoTouchQuery, 1);
    assert_eq!(counter_value(&server, "down"), 1.0, "one tick, whole chain");
}

/// `InputPressIn` / `InputPressOut`, which four shipped connections each fire.
/// Nobody has to be standing on the pad.
#[test]
fn the_press_inputs_work_with_nobody_on_the_pad() {
    let mut server = Server::new();
    server.level_init("test", &button_map(&[]), &[]);
    let button = find_named(&server, "pad").id();

    server.accept_input(button, "PressIn", Variant::Void, None, None, 0);
    run(&mut server, 0.1);
    assert!(pad(&server).pressed);
    assert_eq!(counter_value(&server, "down"), 1.0);

    server.accept_input(button, "PressOut", Variant::Void, None, None, 0);
    run(&mut server, 0.1);
    assert!(!pad(&server).pressed);
    assert_eq!(counter_value(&server, "up"), 1.0);
}

/// A turned pad notices a turned area. 23 of the game's 65 are not at
/// `angles "0 0 0"`, including the one on `sp_a1_intro1`, so this is the
/// ordinary case rather than the exotic one — and it is the path
/// [`obb`](crate::server::obb)'s fifteen planes exist for.
#[test]
fn a_turned_pad_notices_a_turned_area() {
    // Straight out along +X, 40 units: outside an unturned pad (which reaches
    // 20) and inside one turned 45 degrees (whose corner reaches 28.3).
    let probe = Vec3::new(40.0, 0.0, 0.0);

    for (angles, expect) in [("0 0 0", false), ("0 45 0", true)] {
        let mut server = Server::new();
        server.level_init(
            "test",
            &button_map(&[("origin", "0 0 0"), ("angles", angles)]),
            &[],
        );
        server.spawn_player(player_at(Vec3::new(500.0, 0.0, 0.0)));
        run_touching(&mut server, &mut NoTouchQuery, 0.2);

        server.set_player_state(player_at(probe));
        run_touching(&mut server, &mut NoTouchQuery, 0.1);
        assert_eq!(pad(&server).pressed, expect, "angles {angles}");
    }
}

/// `SetSkin( button_off_skin )` runs in `Spawn`, *after* the `skin` key has
/// been read — so a map cannot choose the starting skin. All 18 shipped
/// buttons that write the key write `0`, so nothing in the game can tell, and
/// this is the test that says the order is Valve's rather than an accident.
#[test]
fn the_skin_key_is_read_and_then_overwritten_by_spawn() {
    let mut server = Server::new();
    server.level_init("test", &button_map(&[("skin", "1")]), &[]);
    assert_eq!(pad(&server).skin, 0);

    // …and the skin *input* is not overwritten, because nothing runs after it.
    let button = find_named(&server, "pad").id();
    server.accept_input(button, "skin", Variant::Int(3), None, None, 0);
    run(&mut server, 0.1);
    assert_eq!(pad(&server).skin, 3);

    // Pressing sets it, which is the only thing that reads it here.
    server.accept_input(button, "PressIn", Variant::Void, None, None, 0);
    run(&mut server, 0.1);
    assert_eq!(pad(&server).skin, 1);
}

/// A button with no `model` key gets `PROP_FLOOR_BUTTON_DEFAULT_MODEL_NAME`.
/// No shipped map reaches this — all 77 write one — but `GetButtonModelName`
/// is two lines and the branch is the interesting one.
#[test]
fn a_button_with_no_model_gets_the_default_one() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[("classname", "prop_floor_button"), ("targetname", "pad")]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    assert_eq!(
        find_named(&server, "pad").model.as_deref(),
        Some("models/props/portal_button.mdl")
    );
}

/// Every `prop_floor_button` in a loaded level, in lump order.
fn buttons_in(server: &Server) -> Vec<crate::server::entity::EntityId> {
    server
        .entities
        .iter()
        .filter(|(_, e)| e.classname() == "prop_floor_button")
        .map(|(id, _)| id)
        .collect()
}

/// Is the `ordinal`-th button in the level down?
fn is_pressed(server: &Server, ordinal: usize) -> bool {
    let id = buttons_in(server)[ordinal];
    server
        .entities
        .get(id)
        .expect("just listed")
        .behaviour
        .downcast_ref::<classes::FloorButton>()
        .expect("a FloorButton")
        .pressed
}

/// **Every `prop_floor_button` in the game, stood on.**
///
/// The `SOLID_OBB` half of what
/// [`every_shipped_maps_triggers_notice_the_player`] does for brush triggers,
/// and the test that could not be faked: it uses the real placements, the real
/// angles — 23 of the 65 are turned, one of them to `44.9997 0 90.0004` — and
/// the same `player_touch_triggers` pass the running game uses. A wrong
/// `angle_matrix` convention, a trigger that never got created, a bad
/// `Solid::Obb`, or a fifteen-plane sweep that disagrees with the
/// axis-aligned one all fail here and are invisible to every synthetic test.
///
/// It needs no collision data at all, which is the whole point of
/// [`obb`](crate::server::obb): a box trigger is answered inside `server/`.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release floor_button_presses -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_floor_button_presses_when_stood_on() {
    use crate::engine::world::bsp::Bsp;
    use crate::filesystem::Vfs;

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

    let (mut buttons, mut maps_with_one, mut pressed, mut released, mut turned) = (0, 0, 0, 0, 0);

    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let entities = bsp.entities();

        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);

        // Where every button is, and which way it faces. Indexed by position
        // in the entity list rather than by name: **22 of the 65 have no
        // `targetname`**, and slot order is lump order and is stable across
        // the reloads below.
        let placements: Vec<(Vec3, Vec3)> = buttons_in(&server)
            .iter()
            .map(|&id| {
                let e = server.entities.get(id).expect("just listed");
                (e.core.origin, e.core.angles)
            })
            .collect();
        if placements.is_empty() {
            continue;
        }
        maps_with_one += 1;

        for (ordinal, (origin, angles)) in placements.into_iter().enumerate() {
            buttons += 1;
            turned += usize::from(angles != Vec3::ZERO);

            // **A fresh level per button**, for the same reason the trigger
            // test reloads: a map's own bootstrap can move or kill things, and
            // one button's press can change another's world.
            server.level_init(name, &entities, &bsp.models);

            // Put the player's *hull centre* on the pad's box centre, which is
            // a point inside the trigger whichever way the pad faces — some of
            // these are on walls. The feet are 36 units below it.
            let centre = origin + crate::math::angle_matrix(angles) * Vec3::new(0.0, 0.0, 7.0)
                - Vec3::new(0.0, 0.0, 36.0);

            server.spawn_player(player_at(centre + Vec3::new(0.0, 0.0, 4096.0)));
            ticks_touching(&mut server, &mut NoTouchQuery, 2);

            server.set_player_state(player_at(centre));
            ticks_touching(&mut server, &mut NoTouchQuery, 2);
            if is_pressed(&server, ordinal) {
                pressed += 1;
            } else {
                println!("    MISS {name} #{ordinal} at {origin:?} {angles:?}");
                continue;
            }

            // …and walking away releases it. 4,096 units up is well clear of
            // any pad and is where the player started.
            server.set_player_state(player_at(centre + Vec3::new(0.0, 0.0, 4096.0)));
            ticks_touching(&mut server, &mut NoTouchQuery, 2);
            released += usize::from(!is_pressed(&server, ordinal));
        }
    }

    println!("\n{} maps", names.len());
    println!(
        "  {buttons} floor buttons in {maps_with_one} maps ({turned} of them turned); \
         {pressed} pressed when stood on, {released} released on the way off"
    );

    assert_eq!(names.len(), 106);
    assert_eq!(buttons, 65, "prop_floor_button, across the shipped maps");
    assert_eq!(maps_with_one, 47);
    // 23 are not at `angles "0 0 0"` and therefore take `obb`'s fifteen-plane
    // path rather than its axis-aligned one — including the one on
    // `sp_a1_intro1`, which is at yaw 90.
    assert_eq!(turned, 23);
    // **No room for a partial answer.** A button a player is standing inside
    // presses, and one they have left comes back up.
    assert_eq!(pressed, 65, "a button did not notice a player on it");
    assert_eq!(released, 65, "a button did not come back up");
}

/// The thing that stands on the pad is the **activator** of `OnPressed`, all
/// the way across the extra queue hop the trigger adds.
///
/// `m_OnPressed.FireOutput( pActivator, this )` — the activator is the
/// toucher and the caller is the button, and a map that reads `!activator` off
/// a button gets the player. Pinned with `Kill` at `!activator`, which needs no
/// class beyond `CBaseEntity`: if the wrong entity arrives, the wrong one dies.
#[test]
fn the_thing_standing_on_the_pad_is_the_activator() {
    let mut map = button_map(&[("origin", "0 0 0")]);
    // `OnPressed !activator:Kill`.
    map[1].pairs.push((
        "OnPressed".to_owned(),
        conn("!activator", "Kill", "", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    server.spawn_player(player_at(Vec3::new(500.0, 0.0, 0.0)));
    run_touching(&mut server, &mut NoTouchQuery, 0.2);
    assert!(server.player().is_some());

    server.set_player_state(player_at(Vec3::ZERO));
    run_touching(&mut server, &mut NoTouchQuery, 0.1);
    assert!(
        server.player().is_none(),
        "`!activator` off a button must be whoever stood on it"
    );
    // …and the button still fired its ordinary output, with itself as caller.
    assert_eq!(counter_value(&server, "down"), 1.0);
}

// ---------------------------------------------------------------------------
// stage 5: the player as an entity — health, death, and the move type
// ---------------------------------------------------------------------------

/// The player's health, for the tests below.
fn player_health(server: &Server) -> i32 {
    server
        .entities
        .get(server.player().expect("a player"))
        .expect("alive")
        .core
        .health
}

/// The player's life state.
fn player_life_state(server: &Server) -> crate::server::damage::LifeState {
    server
        .entities
        .get(server.player().expect("a player"))
        .expect("alive")
        .core
        .life_state
}

/// A map with one lethal `trigger_hurt` over the origin, wired to a counter.
fn lethal_hurt_map() -> Vec<bsp::Entity> {
    let mut map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "trigger_hurt"),
            ("model", "*1"),
            ("spawnflags", "1"),
            // The commonest lethal value in the shipped game: 53 of the 215
            // write exactly this.
            ("damage", "500"),
            // `DMG_FALL`, which 34 of them write — the goo and the pits.
            ("damagetype", "32"),
        ]),
        block(&[("classname", "math_counter"), ("targetname", "count")]),
    ];
    map[1].pairs.push((
        String::from("OnHurtPlayer"),
        conn("count", "Add", "1", "0", "-1"),
    ));
    map
}

/// **The headline of the stage**: a `trigger_hurt` kills, and the map that
/// held the corpse asks the engine to start again.
///
/// The whole chain in one test — `HurtEntity` → `Context::take_damage` →
/// `Server::flush_damage` → `CPortal_Player::OnTakeDamage` → `Event_Killed` →
/// `Event_Dying` → `PlayerDeathThink` → `RespawnPlayer` → the level restart —
/// with the timings the shipped game has at every step.
#[test]
fn a_lethal_trigger_kills_the_player_and_asks_for_the_level_back() {
    let mut server = Server::new();
    server.level_init("test", &lethal_hurt_map(), &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));
    assert_eq!(player_health(&server), 100);
    assert_eq!(player_life_state(&server), LifeState::Alive);

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);

    // One tick is enough: `Touch` arms the think at `curtime` and the touch
    // pass runs before the thinks, so the first dose of 250 lands immediately.
    ticks_touching(&mut server, &mut query, 1);
    assert_eq!(counter_value(&server, "count"), 1.0);
    assert!(player_health(&server) <= 0, "500 a second kills at once");
    assert_eq!(player_life_state(&server), LifeState::Dying);

    // `Event_Killed`'s three observable acts.
    let state = server.player_state().expect("still in the list");
    assert_eq!(
        state.move_type,
        crate::server::movement::MoveType::FlyGravity,
        "the corpse falls"
    );
    assert!(!state.on_ground, "SetGroundEntity( NULL )");
    assert!(
        !server
            .entities
            .get(server.player().expect("a player"))
            .expect("alive")
            .core
            .is_solid(),
        "a corpse is walked through — FSOLID_NOT_SOLID"
    );

    // > **The trigger keeps firing `OnHurtPlayer` at the corpse, and that is
    // > Valve's.** Three things that each look like they would stop it do not:
    // > the player going `FSOLID_NOT_SOLID` only stops the *player* testing
    // > triggers (`PhysicsTouchTriggers` returns early), and a stationary
    // > `MOVETYPE_NONE` trigger never re-tests its own touches, so the touch
    // > *link* survives; `m_takedamage` stays `DAMAGE_YES`, because
    // > `CBaseCombatCharacter::Event_Killed` does **not** chain to
    // > `CBaseEntity::Event_Killed`, which is the one that would clear it; and
    // > `HurtEntity` fires its output before it knows whether the damage
    // > landed, because `TakeDamage` returns `void`.
    // >
    // > So for the three seconds between dying and the reload, a map's
    // > `OnHurtPlayer` chain runs six more times. It is bounded, it is what
    // > the shipped game does, and a port that "fixed" it would run a
    // > different amount of map logic than the game.
    let health = player_health(&server);
    run_touching(&mut server, &mut query, 2.0);
    assert_eq!(
        counter_value(&server, "count"),
        5.0,
        "1 + 2 seconds of them"
    );
    assert_eq!(player_health(&server), health, "and takes nothing more");

    // `sp_fade_and_force_respawn` is 1: three seconds after the death, the
    // level is asked for again. Not before.
    assert!(server.take_level_restart().is_none(), "not yet");
    run_touching(&mut server, &mut query, 1.2);
    assert_eq!(
        server.take_level_restart().as_deref(),
        Some("test"),
        "RespawnPlayer -> respawn() -> reload"
    );
    assert!(
        server.take_level_restart().is_none(),
        "taken once, not once a tick"
    );
}

/// `god` refuses the damage — **and the outputs still fire**, because
/// `TakeDamage` returns `void` and `HurtEntity` cannot see a refusal.
///
/// That is Valve's, and it is what makes the cheat usable in a scripted
/// chamber rather than a way to wedge one.
#[test]
fn god_mode_refuses_the_damage_and_the_outputs_still_fire() {
    let mut server = Server::new();
    server.level_init("test", &lethal_hurt_map(), &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));
    assert_eq!(server.toggle_god(), Some(true));

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 1.1);

    assert_eq!(player_health(&server), 100, "FL_GODMODE");
    assert_eq!(player_life_state(&server), LifeState::Alive);
    assert_eq!(counter_value(&server, "count"), 3.0, "OnHurtPlayer still");

    // …and off again. Half a second, because the half-second `HurtThink` is
    // already armed and the next dose is due whenever it is due.
    assert_eq!(server.toggle_god(), Some(false));
    run_touching(&mut server, &mut query, 0.6);
    assert!(player_health(&server) <= 0, "and off again");
}

/// `kill` — `CBasePlayer::CommitSuicide`, and its five-second cooldown.
#[test]
fn the_kill_command_kills_once_and_then_refuses() {
    let mut server = Server::new();
    server.level_init("test", &lethal_hurt_map(), &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    assert!(server.kill_player());
    assert_eq!(player_health(&server), 0, "set, not subtracted");
    assert_eq!(player_life_state(&server), LifeState::Dying);
    // Already dead, so `IsAlive()` refuses before the cooldown is even read.
    assert!(!server.kill_player());
}

/// `hurtme` and the fractional accumulator, end to end: five hits of 2.5 take
/// 13 points, not 10.
#[test]
fn fractional_damage_accumulates_across_calls() {
    let mut server = Server::new();
    server.level_init("test", &lethal_hurt_map(), &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));

    for _ in 0..5 {
        assert!(server.hurt_player(2.5, crate::server::damage::DMG_GENERIC));
    }
    // 2, 3, 2, 3, 2 — the accumulator pays out on the second hit and the
    // fourth, so five hits of 2.5 are 12 points and not 10.
    assert_eq!(player_health(&server), 88);
}

/// A `damagefilter` on the victim refuses the damage before it is queued.
///
/// Two things this pins. The **`==`** in `FilterDamageType`: a filter for
/// `DMG_BURN` refuses `DMG_FALL`, which is what the trigger deals, so the
/// damage never lands. And the *outputs still fire*, for the same reason god
/// mode's do.
#[test]
fn a_damage_filter_on_the_victim_refuses_the_damage() {
    let mut map = lethal_hurt_map();
    map.push(block(&[
        ("classname", "filter_damage_type"),
        ("targetname", "only_burns"),
        // `DMG_BURN`, which is not what the trigger deals.
        ("damagetype", "8"),
    ]));

    let mut server = Server::new();
    server.level_init("test", &map, &trigger_models());
    server.spawn_player(player_at(Vec3::ZERO));
    // The player is not in the `.bsp`, so its filter is set the way the one
    // input that can set one does.
    let player = server.player().expect("a player");
    server.dispatch(player, |core, _behaviour, cx| {
        core.damage_filter = cx.find_by_name("only_burns");
    });
    assert!(server
        .entities
        .get(player)
        .expect("alive")
        .core
        .damage_filter
        .is_some());

    let (mins, maxs) = trigger_box();
    let mut query = BoxTriggers::new(&[(1, mins, maxs)]);
    run_touching(&mut server, &mut query, 1.1);

    assert_eq!(player_health(&server), 100, "DMG_FALL is not DMG_BURN");
    assert_eq!(counter_value(&server, "count"), 3.0);
}

/// `noclip` is a **server** command again — `portdocs/CLIENT.md` §9.2's wart,
/// closed — and `Server::set_player_state` must not undo it.
#[test]
fn noclip_is_the_servers_and_survives_the_round_trip() {
    let mut server = Server::new();
    server.level_init("test", &lethal_hurt_map(), &trigger_models());
    let state = player_at(Vec3::ZERO);
    server.spawn_player(state);

    assert_eq!(server.toggle_noclip(), Some(true));
    // The client's copy arriving at the top of the next rendered frame still
    // says `MOVETYPE_WALK`, because the client has not been told yet. If
    // `set_player_state` wrote the move type, this line would turn noclip off
    // a fraction of a frame after it was turned on.
    server.set_player_state(state);
    assert_eq!(
        server.player_state().expect("a player").move_type,
        crate::server::movement::MoveType::Noclip
    );
    assert_eq!(server.toggle_noclip(), Some(false));
}

/// Jumping and ducking fire `logic_playerproxy`'s outputs.
///
/// **Every one of the five proxy output connections in the shipped game is on
/// `sp_a1_intro1`**, which is the map this port loads by default: three
/// `OnJump` and one each of `OnDuck` and `OnUnDuck`.
#[test]
fn jumping_and_ducking_reach_the_player_proxy() {
    let mut map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "logic_playerproxy"),
            ("targetname", "playerproxy"),
        ]),
        block(&[("classname", "math_counter"), ("targetname", "jumps")]),
        block(&[("classname", "math_counter"), ("targetname", "ducks")]),
        block(&[("classname", "math_counter"), ("targetname", "unducks")]),
    ];
    map[1]
        .pairs
        .push((String::from("OnJump"), conn("jumps", "Add", "1", "0", "-1")));
    map[1]
        .pairs
        .push((String::from("OnDuck"), conn("ducks", "Add", "1", "0", "-1")));
    map[1].pairs.push((
        String::from("OnUnDuck"),
        conn("unducks", "Add", "1", "0", "-1"),
    ));

    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    let mut state = player_at(Vec3::ZERO);
    server.spawn_player(state);

    let mut query = crate::server::NoTouchQuery;
    let interval = server.time().interval;

    // Holding jump fires **once**, on the press edge — `m_afButtonPressed`,
    // not `m_nButtons`. A held key that re-fired every tick would send 64
    // relays a second.
    state.buttons = crate::server::classes::IN_JUMP;
    for _ in 0..4 {
        server.set_player_state(state);
        server.frame(interval, &mut query);
    }
    assert_eq!(counter_value(&server, "jumps"), 1.0);

    // Release and press again: a second jump.
    state.buttons = 0;
    server.set_player_state(state);
    server.frame(interval, &mut query);
    state.buttons = crate::server::classes::IN_JUMP;
    server.set_player_state(state);
    server.frame(interval, &mut query);
    assert_eq!(counter_value(&server, "jumps"), 2.0);

    // Ducking is the button; *un*ducking is the **hull**, because
    // `CPortal_Player::UnDuck` is called when the box grows back rather than
    // when the key comes up.
    state.buttons = crate::server::classes::IN_DUCK;
    state.maxs.z = 36.0;
    server.set_player_state(state);
    server.frame(interval, &mut query);
    assert_eq!(counter_value(&server, "ducks"), 1.0);
    assert_eq!(counter_value(&server, "unducks"), 0.0);

    // Key still down, hull back up — which is what happens at the end of a
    // toggled crouch. `OnUnDuck` fires and `OnDuck` does not fire again.
    state.maxs.z = 72.0;
    server.set_player_state(state);
    server.frame(interval, &mut query);
    assert_eq!(counter_value(&server, "unducks"), 1.0);
    assert_eq!(counter_value(&server, "ducks"), 1.0);
}

/// `player_loadsaved` — Portal 2's *other* way of dying: nine entities, 11
/// `Reload` connections, mostly named `fade_to_death`.
///
/// It takes no health at all. It freezes the player and reloads.
#[test]
fn player_loadsaved_freezes_the_player_and_restarts_the_level() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "player_loadsaved"),
            ("targetname", "fade_to_death"),
            // The value seven of the nine shipped ones carry.
            ("loadtime", "2.5"),
            ("holdtime", "100"),
            ("duration", ".5"),
        ]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &[]);
    server.spawn_player(player_at(Vec3::ZERO));

    let mut query = crate::server::NoTouchQuery;
    server.queue.add(Event {
        fire_time: 0.0,
        target: Target::Name(String::from("fade_to_death")),
        input: String::from("Reload"),
        value: Variant::Void,
        activator: None,
        caller: None,
        output_id: 0,
    });
    server.frame(server.time().interval, &mut query);

    assert_eq!(player_health(&server), 100, "it is not damage");
    assert!(
        server.player_state().expect("a player").flags & crate::server::movement::FL_FROZEN != 0,
        "\"Adrian: Setting this flag so we can't move or save a game.\""
    );
    assert!(server.take_level_restart().is_none(), "not for 2.5 seconds");

    run_touching(&mut server, &mut query, 2.6);
    assert_eq!(server.take_level_restart().as_deref(), Some("test"));
}

/// The `health` key is consumed rather than counted as unhandled — 682 of the
/// game's entities carry it and **every one writes `0`**, which is why no
/// shipped door or button is shootable.
#[test]
fn the_health_key_is_read_and_the_shipped_value_makes_nothing_damageable() {
    let map = vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "func_door"),
            ("targetname", "door"),
            ("model", "*1"),
            ("health", "0"),
        ]),
    ];
    let mut server = Server::new();
    let stats = server.level_init("test", &map, &trigger_models());
    assert!(!stats.unhandled.contains_key("health"));

    let door = find_named(&server, "door");
    assert_eq!(door.core.health, 0);
    assert!(
        !door.core.take_damage.takes_damage(),
        "CBaseDoor::Spawn only sets DAMAGE_YES above zero"
    );
}

/// **Every `trigger_hurt` in the shipped game, against a player standing in
/// it** — the measurement that says `portdocs/SERVER.md` stage 5 works.
///
/// The sibling of [`every_shipped_maps_triggers_notice_the_player`], and the
/// same shape: per map it builds the real collision, and then for each
/// `trigger_hurt` it reloads the level, finds a point inside the trigger's
/// *actual brushes* that a 32×32×72 hull fits in, puts a player there and runs
/// the clock until the player dies or sixteen seconds pass.
///
/// Sixteen seconds is chosen rather than guessed. The weakest `damage` key in
/// the game is 10, dealt every half second against 100 health, so the slowest
/// possible death is ten seconds and change — and the level restart it asks
/// for comes three seconds after that.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release trigger_hurt_kills -- --ignored --nocapture
/// ```
///
/// [`every_shipped_maps_triggers_notice_the_player`]: fn@every_shipped_maps_triggers_notice_the_player
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_trigger_hurt_kills_the_player_standing_in_it() {
    use crate::engine::trace::CollisionBsp;
    use crate::engine::world::{bsp::Bsp, find_brush_models, PlacedBrushModel};

    /// How long to stand in each trigger. See the docs.
    const SECONDS: f32 = 16.0;

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

    let mut visited = 0usize;
    let mut killed = 0usize;
    // No point inside the brushes a standing hull fits in.
    let mut unreachable = 0usize;
    // `StartDisabled 1` — 73 of the game's 215 carry it, and a disabled
    // trigger is `FSOLID_NOT_SOLID` without `FSOLID_TRIGGER`, so nothing ever
    // touches it. The map switches them on with an `Enable`.
    let mut disabled = 0usize;
    // Touched, and the trigger refused to hurt what touched it —
    // `PassesTriggerFilters`. Five of the game's `trigger_hurt`s do not carry
    // `SF_TRIGGER_ALLOW_CLIENTS`, and one has a `filtername` that names an
    // `npc_bullseye`.
    let mut refused = 0usize;
    // How long each death took, in seconds of server time.
    let mut times: Vec<f32> = Vec::new();
    let mut restarts = 0usize;

    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let collision = CollisionBsp::build(&bsp);
        let entities = bsp.entities();
        let placed = find_brush_models(&entities, &collision);

        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);

        let mut targets: Vec<(usize, Vec3, Vec3)> = Vec::new();
        for (_, entity) in server.entities.iter() {
            if entity.classname() != "trigger_hurt" {
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
                entity.core.origin + bounds.mins,
                entity.core.origin + bounds.maxs,
            ));
        }

        let owned: Vec<PlacedBrushModel> = placed
            .iter()
            .filter(|p| server.brush_entity(p.index).is_some())
            .cloned()
            .map(|mut p| {
                p.owned = true;
                p
            })
            .collect();

        for (index, mins, maxs) in targets {
            // A fresh level per trigger, for the same reason the sibling test
            // does it: a map's own bootstrap can change what is there.
            server.level_init(name, &entities, &bsp.models);
            visited += 1;

            let Some(probe) = probe_inside(&collision, &placed, index, mins, maxs) else {
                unreachable += 1;
                continue;
            };

            server.spawn_player(player_at(probe));
            let mut query = Placed {
                collision: &collision,
                models: &owned,
                chain: Vec::new(),
            };
            let interval = server.time().interval;
            let ticks = (SECONDS / interval).round() as u32;
            let mut died_at = None;
            let mut touched = false;
            for tick in 0..ticks {
                server.frame(interval, &mut query);
                // **The player never moves.** A standing hull is put inside
                // the trigger and left there, so anything that happens is the
                // trigger's doing.
                touched = touched
                    || server
                        .brush_entity(index)
                        .is_some_and(|e| !e.touch_links.is_empty());
                if died_at.is_none()
                    && server
                        .player_state()
                        .is_some_and(|s| s.life_state != LifeState::Alive)
                {
                    died_at = Some(tick as f32 * interval);
                }
            }
            if server.take_level_restart().is_some() {
                restarts += 1;
            }
            match died_at {
                Some(at) => {
                    killed += 1;
                    times.push(at);
                }
                None if !touched => disabled += 1,
                None => refused += 1,
            }
        }
    }

    times.sort_by(f32::total_cmp);
    println!("\n{} maps", names.len());
    println!(
        "  {visited} trigger_hurts visited, {killed} killed the player;\n  \
         {disabled} were never touched (StartDisabled), \
         {refused} touched and refused (spawnflags or a filter), \
         {unreachable} had no point a standing player fits in"
    );
    if let (Some(first), Some(last)) = (times.first(), times.last()) {
        let median = times[times.len() / 2];
        println!("  time to die: {first:.2}s fastest, {median:.2}s median, {last:.2}s slowest");
    }
    println!("  {restarts} of the deaths asked the engine for the level back");

    assert_eq!(names.len(), 106);
    assert_eq!(visited, 215, "the game places 215 trigger_hurts");
    // **Nothing is unexplained.** Every trigger a standing player fits inside,
    // that is switched on, and that admits clients, must kill them — the
    // weakest damage value in the game is 10 a second and nothing heals.
    assert_eq!(
        killed + disabled + refused + unreachable,
        visited,
        "a trigger_hurt failed to kill a player standing in it for {SECONDS}s"
    );
    // 138 of the game's 215 `trigger_hurt`s kill a player who stands in them,
    // and every one of the other 77 is accounted for:
    //
    // - **72 are never touched.** 73 of the 215 carry `StartDisabled 1` — a
    //   disabled trigger is `FSOLID_NOT_SOLID` *without* `FSOLID_TRIGGER`, so
    //   nothing can touch it until the map sends it an `Enable`. (72 rather
    //   than 73 because one of them is switched on by its own map's bootstrap
    //   within the sixteen seconds, and then refuses for the next reason.)
    // - **4 are touched and refuse**, which is `PassesTriggerFilters`: five of
    //   the game's `trigger_hurt`s do not carry `SF_TRIGGER_ALLOW_CLIENTS`
    //   (spawnflags 4098 and 5128), and one names a `filtername` that resolves
    //   to an `npc_bullseye` filter.
    // - **1 has nowhere to stand** — no point inside its brushes that a
    //   32×32×72 hull fits in.
    assert_eq!(killed, 138);
    assert_eq!(disabled, 72, "StartDisabled 1");
    assert_eq!(refused, 4, "no SF_TRIGGER_ALLOW_CLIENTS, or a filter");
    assert_eq!(unreachable, 1);
    // **Every death reaches `RespawnPlayer`**, three seconds later — which is
    // inside the sixteen for every one of them.
    assert_eq!(restarts, killed, "a death did not reach RespawnPlayer");
}

// ===========================================================================
// prop_dynamic — the model the map places, and animates
// ===========================================================================

/// A map with one `prop_dynamic` named `prop`, wired so that both of its
/// animation outputs land on a counter.
///
/// `extra` goes on the prop, which is how these tests choose its classname's
/// siblings, its `solid`, and its `DefaultAnim`.
fn prop_map(extra: &[(&str, &str)]) -> Vec<bsp::Entity> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("classname", "prop_dynamic"),
        ("targetname", "prop"),
        ("model", "models/props/panel.mdl"),
    ];
    for (key, value) in extra {
        match pairs.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            // A `classname` in `extra` *replaces* the default rather than
            // being a second one — the lump is a list of pairs, and the first
            // `classname` is the one that chooses the class.
            Some(pair) => pair.1 = value,
            None => pairs.push((key, value)),
        }
    }
    let mut prop = block(&pairs);
    prop.pairs.push((
        "OnAnimationBegun".to_owned(),
        conn("begun", "Add", "1", "0", "-1"),
    ));
    prop.pairs.push((
        "OnAnimationDone".to_owned(),
        conn("done", "Add", "1", "0", "-1"),
    ));

    vec![
        block(&[("classname", "worldspawn")]),
        prop,
        block(&[("classname", "math_counter"), ("targetname", "begun")]),
        block(&[("classname", "math_counter"), ("targetname", "done")]),
    ]
}

/// The pose `engine::world::entities` would compute, against a sequence of
/// `duration` seconds that does not loop.
///
/// **The renderer's arithmetic, written out here**, because the seam is five
/// numbers and the whole point of this class is that the two sides agree on
/// what they mean.
fn prop_cycle(server: &Server, duration: f32) -> f32 {
    let state = prop_of(server).model_state().expect("it draws a model");
    let elapsed = (server.time().curtime - state.anim_time).max(0.0);
    (state.cycle + elapsed * state.playback_rate / duration).clamp(0.0, 1.0)
}

fn prop_of(server: &Server) -> &classes::DynamicProp {
    find_named(server, "prop")
        .behaviour
        .downcast_ref::<classes::DynamicProp>()
        .expect("a DynamicProp")
}

/// A table saying `open` takes a second and does not loop, and `spin` loops.
fn panel_sequences() -> sequences::SequenceTable {
    let mut table = sequences::SequenceTable::new();
    table.insert_model(
        "models/props/panel.mdl",
        [
            (
                "open".to_owned(),
                sequences::SequenceInfo {
                    duration: 1.0,
                    loops: false,
                    fade_out_time: 0.2,
                },
            ),
            (
                "open_idle".to_owned(),
                sequences::SequenceInfo {
                    duration: 1.0,
                    loops: false,
                    fade_out_time: 0.2,
                },
            ),
            (
                "spin".to_owned(),
                sequences::SequenceInfo {
                    duration: 2.0,
                    loops: true,
                    fade_out_time: 0.2,
                },
            ),
        ],
    );
    table
}

/// `CBaseProp::Spawn`'s resting state, and the promotion that is the whole
/// difference between the two classnames a map can place.
#[test]
fn a_dynamic_prop_spawns_still_and_a_plain_one_is_promoted_to_an_obb() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[("solid", "0")]), &[]);

    let prop = find_named(&server, "prop");
    // `SetSolid( SOLID_OBB ); AddSolidFlags( FSOLID_NOT_SOLID )`.
    assert_eq!(prop.solid, crate::server::movement::Solid::Obb);
    assert!(!prop.is_solid(), "you walk through a prop_dynamic");
    // `CBaseProp::Spawn`.
    assert_eq!(prop.move_type, crate::server::movement::MoveType::Push);
    assert_eq!(prop.health, 0);
    assert_eq!(prop.max_health, 1);
    assert_eq!(
        prop.take_damage,
        crate::server::damage::DamageMode::EventsOnly
    );
    // A mover with no alarm is not in the simulation list — 8,462 props in the
    // game would otherwise be.
    assert!(!prop.will_simulate_game_physics());

    // **The rate is zero**, so a prop with no `DefaultAnim` holds frame zero
    // for ever rather than looping sequence 0.
    let state = prop.behaviour.model_state().expect("it draws a model");
    assert_eq!(state.playback_rate, 0.0);
    assert_eq!(state.sequence, "");

    // And an `_override` with the same `solid 0` is *not* promoted — 211 of
    // the game's 390 are in exactly this position.
    let mut server = Server::new();
    server.level_init(
        "test",
        &prop_map(&[("classname", "prop_dynamic_override"), ("solid", "0")]),
        &[],
    );
    assert_eq!(
        find_named(&server, "prop").solid,
        crate::server::movement::Solid::None,
        "prop_dynamic_override keeps SOLID_NONE"
    );
}

/// `solid 6` survives `Spawn`, because the prop family is the one family whose
/// `Spawn` does not call `SetSolid`.
#[test]
fn the_solid_key_reaches_a_prop_and_no_other_class_writes_one() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[("solid", "6")]), &[]);
    assert_eq!(
        find_named(&server, "prop").solid,
        crate::server::movement::Solid::VPhysics
    );
    // `SF_DYNAMICPROP_DISABLE_COLLISION`, which 161 shipped props set.
    let mut server = Server::new();
    server.level_init(
        "test",
        &prop_map(&[("solid", "6"), ("spawnflags", "256")]),
        &[],
    );
    assert!(!find_named(&server, "prop").is_solid());
}

/// **A prop with no model deletes itself**, which is `CBaseProp::Spawn`'s
/// first four lines. No shipped map reaches it; a hand-made one would.
#[test]
fn a_prop_with_no_model_removes_itself() {
    let mut server = Server::new();
    let stats = server.level_init(
        "test",
        &[
            block(&[("classname", "worldspawn")]),
            block(&[("classname", "prop_dynamic"), ("targetname", "prop")]),
        ],
        &[],
    );
    assert_eq!(stats.matched, 2);
    assert_eq!(stats.removed_on_spawn, 1);
}

/// **`SetAnimation` plays a sequence, and `OnAnimationDone` fires when it
/// ends** — the two halves of what 5,311 and 181 shipped connections do.
#[test]
fn set_animation_plays_a_sequence_and_fires_both_of_its_outputs() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[]), &[]);
    server.set_sequences(panel_sequences());
    run(&mut server, 0.5);
    assert_eq!(
        counter_value(&server, "begun"),
        0.0,
        "nothing has begun yet"
    );

    let prop_id = find_named(&server, "prop").id();
    server.accept_input(
        prop_id,
        "SetAnimation",
        Variant::String("open".to_owned()),
        None,
        None,
        0,
    );
    run(&mut server, 0.1);

    // `PropSetAnim` fires `OnAnimationBegun` and `FinishSetSequence` starts
    // the sequence forwards from zero at rate 1.
    assert_eq!(counter_value(&server, "begun"), 1.0);
    let state = prop_of(&server).model_state().expect("it draws a model");
    assert_eq!(state.sequence, "open");
    assert_eq!(state.cycle, 0.0);
    assert_eq!(state.playback_rate, 1.0);

    // Half a second in, nothing has finished.
    run(&mut server, 0.4);
    assert_eq!(counter_value(&server, "done"), 0.0);

    // A second in, it has — once, and only once however long we wait.
    run(&mut server, 0.7);
    assert_eq!(counter_value(&server, "done"), 1.0);
    run(&mut server, 2.0);
    assert_eq!(counter_value(&server, "done"), 1.0, "it fires once");
}

/// **A finished animation reverts to `DefaultAnim`**, which is what puts a
/// panel that was told to `open` into `open_idle` without the map saying so —
/// and `HoldAnimation` is what stops it. 2,416 props carry the first and 857
/// the second.
#[test]
fn a_finished_animation_reverts_to_the_default_unless_the_prop_holds_it() {
    for (hold, expected) in [("0", "open_idle"), ("1", "open")] {
        let mut server = Server::new();
        server.level_init(
            "test",
            &prop_map(&[("DefaultAnim", "open_idle"), ("HoldAnimation", hold)]),
            &[],
        );
        server.set_sequences(panel_sequences());

        // `Spawn`'s own `PropSetAnim( DefaultAnim )` — which ran against an
        // empty table and was believed anyway.
        assert_eq!(
            prop_of(&server)
                .model_state()
                .expect("it draws a model")
                .sequence,
            "open_idle"
        );

        let prop_id = find_named(&server, "prop").id();
        server.accept_input(
            prop_id,
            "SetAnimation",
            Variant::String("open".to_owned()),
            None,
            None,
            0,
        );
        run(&mut server, 0.1);
        assert_eq!(
            prop_of(&server)
                .model_state()
                .expect("it draws a model")
                .sequence,
            "open"
        );

        run(&mut server, 1.2);
        assert_eq!(
            prop_of(&server)
                .model_state()
                .expect("it draws a model")
                .sequence,
            expected,
            "HoldAnimation {hold}"
        );
        // Either way the end was noticed exactly once.
        assert_eq!(counter_value(&server, "done"), 1.0);
    }
}

/// A **looping** sequence never finishes, so nothing fires — and the think
/// stops rather than waking the entity ten times a second for the rest of the
/// level.
#[test]
fn a_looping_sequence_never_finishes_and_stops_thinking() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[("DefaultAnim", "spin")]), &[]);
    server.set_sequences(panel_sequences());
    run(&mut server, 5.0);

    assert_eq!(counter_value(&server, "begun"), 1.0, "Spawn's PropSetAnim");
    assert_eq!(counter_value(&server, "done"), 0.0, "a loop never ends");
    assert_eq!(
        find_named(&server, "prop").next_think_tick(),
        think::TICK_NEVER_THINK,
        "the think has nothing left to decide"
    );

    // And the flag is reset on the way, so a *later* sequence that does finish
    // still fires. This is the one line of Valve's `else` branch that had to
    // survive the think being cancelled.
    let prop_id = find_named(&server, "prop").id();
    server.accept_input(
        prop_id,
        "SetAnimation",
        Variant::String("open".to_owned()),
        None,
        None,
        0,
    );
    run(&mut server, 1.3);
    assert_eq!(counter_value(&server, "done"), 1.0);
}

/// **`SetPlaybackRate -1` runs the sequence backwards from where it is** —
/// 427 shipped connections — and the pose does not jump when it arrives.
#[test]
fn set_playback_rate_rebases_the_pose_so_it_does_not_jump() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[]), &[]);
    server.set_sequences(panel_sequences());
    let prop_id = find_named(&server, "prop").id();
    server.accept_input(
        prop_id,
        "SetAnimation",
        Variant::String("open".to_owned()),
        None,
        None,
        0,
    );
    run(&mut server, 0.5);

    // Half a second into a one-second sequence.
    let before = prop_cycle(&server, 1.0);
    assert!((before - 0.5).abs() < 0.05, "half way through: {before}");

    server.accept_input(
        prop_id,
        "SetPlaybackRate",
        Variant::Float(-1.0),
        None,
        None,
        0,
    );
    // The pose is the same one instant later…
    let after = prop_cycle(&server, 1.0);
    assert!(
        (after - before).abs() < 1e-5,
        "the pose jumped: {before} -> {after}"
    );
    assert_eq!(
        prop_of(&server)
            .model_state()
            .expect("it draws a model")
            .playback_rate,
        -1.0
    );
    // …and then runs backwards to the start, where `bPropFinished` is
    // `cycle <= 0` rather than `cycle >= 0.999`.
    run(&mut server, 0.6);
    assert_eq!(counter_value(&server, "done"), 1.0);
    assert_eq!(prop_cycle(&server, 1.0), 0.0);
}

/// `StartDisabled` is `EF_NODRAW`, and `Enable`/`Disable` are Valve's own
/// second names for `TurnOn`/`TurnOff`.
///
/// **The seam carries the invisible prop rather than dropping it**, which is
/// what lets it come back: 1,000 props in the game start this way.
#[test]
fn start_disabled_hides_a_prop_and_enable_brings_it_back() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[("StartDisabled", "1")]), &[]);

    let drawn = |server: &Server| server.model_entities()[0].visible;
    assert_eq!(server.model_entities().len(), 1, "it is in the list");
    assert!(!drawn(&server));

    let prop_id = find_named(&server, "prop").id();
    for (input, visible) in [
        ("Enable", true),
        ("Disable", false),
        ("TurnOn", true),
        ("TurnOff", false),
        ("EnableDraw", true),
        ("DisableDraw", false),
    ] {
        server.accept_input(prop_id, input, Variant::Void, None, None, 0);
        run(&mut server, 0.05);
        assert_eq!(drawn(&server), visible, "{input}");
    }
}

/// `FadeAndKill` takes a second and then the prop is gone — and the seam
/// notices, because it is keyed rather than positional.
#[test]
fn fade_and_kill_removes_the_prop_after_a_second() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[]), &[]);
    // A tick of clearance first, and it is load-bearing: `SUB_StartFadeOut`
    // arms its think for `curtime + 0` and `SetNextThink( 0 )` means **not
    // scheduled**, at tick zero, in this port and in Valve's alike
    // (`physics_main.cpp`'s `thinktick <= 0` guard). No shipped map can fire
    // an input before the first tick.
    run(&mut server, 0.1);
    let prop_id = find_named(&server, "prop").id();
    server.accept_input(prop_id, "FadeAndKill", Variant::Void, None, None, 0);

    run(&mut server, 0.5);
    assert!(
        server.entities.get(prop_id).is_some(),
        "half a second in, it is still fading"
    );
    assert!(
        server.entities.get(prop_id).expect("alive").render_color[3] < 200,
        "and it has faded some of the way"
    );

    run(&mut server, 0.6);
    assert!(server.entities.get(prop_id).is_none());
    assert!(server.model_entities().is_empty());
}

/// `Break` is the only way an `OnBreak` connection can fire in Portal 2 — a
/// prop spawns at `DAMAGE_EVENTS_ONLY` with no health, so nothing can damage
/// it into breaking. 8 shipped connections fire it and 16 listen for the
/// output.
#[test]
fn the_break_input_fires_on_break_and_removes_the_prop() {
    let mut map = prop_map(&[]);
    map[1]
        .pairs
        .push(("OnBreak".to_owned(), conn("done", "Add", "1", "0", "-1")));
    let mut server = Server::new();
    server.level_init("test", &map, &[]);

    let prop_id = find_named(&server, "prop").id();
    server.accept_input(prop_id, "Break", Variant::Void, None, None, 0);
    run(&mut server, 0.1);
    assert_eq!(counter_value(&server, "done"), 1.0);
    assert!(server.entities.get(prop_id).is_none());
}

/// **`health` is swallowed unless the classname is an `_override`** —
/// `CBaseProp::KeyValue`'s one line. All 344 shipped keys write `0`, so this
/// is a test of the mechanism rather than of anything a map does.
#[test]
fn only_an_override_prop_may_be_given_health_by_the_map() {
    for (classname, unhandled) in [("prop_dynamic", 0), ("prop_dynamic_override", 0)] {
        let mut server = Server::new();
        let stats = server.level_init(
            "test",
            &prop_map(&[("classname", classname), ("health", "50")]),
            &[],
        );
        assert_eq!(
            stats.unhandled.get("health"),
            None,
            "{classname} consumed the key either way"
        );
        let _ = unhandled;
        // What differs is whether it was *kept*. Both end at zero, because
        // `CBreakableProp::Spawn` zeroes an unbreakable prop's health — which
        // every Portal 2 prop is.
        assert_eq!(find_named(&server, "prop").health, 0);
    }
}

/// A `SetAnimation` naming a sequence the model does not have warns and stands
/// still, which is Valve's `else` branch — and it is reachable only because
/// the table is filled in.
#[test]
fn a_sequence_the_model_does_not_have_is_refused_once_the_models_are_loaded() {
    let mut server = Server::new();
    server.level_init("test", &prop_map(&[]), &[]);
    server.set_sequences(panel_sequences());

    let prop_id = find_named(&server, "prop").id();
    server.accept_input(
        prop_id,
        "SetAnimation",
        Variant::String("open".to_owned()),
        None,
        None,
        0,
    );
    run(&mut server, 0.1);
    assert_eq!(
        prop_of(&server)
            .model_state()
            .expect("it draws a model")
            .sequence,
        "open"
    );

    server.accept_input(
        prop_id,
        "SetAnimation",
        Variant::String("sideways".to_owned()),
        None,
        None,
        0,
    );
    run(&mut server, 0.1);
    // `SetSequence( 0 )`, which here is the bind pose — and **no**
    // `OnAnimationBegun`, so the counter is still on the one from `open`.
    assert_eq!(
        prop_of(&server)
            .model_state()
            .expect("it draws a model")
            .sequence,
        ""
    );
    assert_eq!(counter_value(&server, "begun"), 1.0);
}

/// **Every `prop_dynamic` in the game, spawned, with its model's real
/// sequences in hand.**
///
/// The class's own depot test, and the one that says what the port can and
/// cannot animate. For each of the 106 maps it spawns the entities, loads
/// every `.mdl` they name — through `StudioModel`, which needs a `Vfs` and no
/// GPU — fills in the sequence table exactly as `Engine::load_level` does, and
/// runs two seconds.
///
/// It is the only test in this file that names a `studio` type, and it does so
/// for the same reason `Engine` does: this is the *seam*, and a seam is only
/// checkable from both sides.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release prop_dynamic -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_prop_dynamic_plays_the_animation_its_map_asks_for() {
    use crate::filesystem::Vfs;
    use crate::studio::StudioModel;

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

    // Every `.mdl` read so far, as the table wants it — `None` for one that
    // would not load, which is an entity that draws nothing.
    let mut loaded: BTreeMap<String, Option<Vec<(String, sequences::SequenceInfo)>>> =
        BTreeMap::new();
    // …and, for each that did load, `(has a skeleton, shares a vertex between
    // bones)`. The second used to decide whether the renderer could pose the
    // model at all — the per-bone draw split could not express a shared vertex
    // — and since skinning landed it decides nothing. It is kept as the
    // measurement that made skinning worth writing, and as the count of what
    // it bought.
    let mut rigidity: BTreeMap<String, (bool, bool)> = BTreeMap::new();

    let mut spawned = 0usize;
    let mut props = 0usize;
    let mut models_missing = 0usize;
    let mut invisible = 0usize;
    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    let mut animating = 0usize;
    let mut animatable = 0usize;
    let mut skinned = 0usize;
    let mut solid_key = [0usize; 2];
    let mut maps_with_a_prop = 0usize;

    let mut server = Server::new();
    for name in &names {
        let bsp = crate::engine::world::bsp::Bsp::load(&vfs, name)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        server.level_init(name, &bsp.entities(), &bsp.models);

        // The engine's job, done here: read every model the entities name and
        // hand the answers back. `Engine::load_level` does exactly this,
        // between `World::load_entity_models` and the first tick.
        let mut table = sequences::SequenceTable::new();
        let mut here = 0usize;
        for (_, entity) in server.entities.iter() {
            if !entity.classname().starts_with("prop_dynamic")
                && entity.classname() != "dynamic_prop"
            {
                continue;
            }
            here += 1;
            let Some(model) = entity.core.model.clone() else {
                continue;
            };
            let entry = loaded.entry(model.to_ascii_lowercase()).or_insert_with(|| {
                let studio = StudioModel::load(&vfs, &model).ok()?;
                rigidity.insert(
                    model.to_ascii_lowercase(),
                    (
                        studio.bones.len() > 1,
                        studio.bones.len() > 1
                            && studio.vertices.iter().any(|v| v.bone_weights[1] != 0.0),
                    ),
                );
                Some(
                    studio
                        .sequences
                        .iter()
                        .enumerate()
                        .map(|(i, sequence)| {
                            let duration = studio
                                .animation(i)
                                .map(|anim| anim.duration())
                                .unwrap_or(0.0);
                            (
                                sequence.label.clone(),
                                sequences::SequenceInfo {
                                    duration,
                                    loops: sequence.flags & crate::studio::anim::STUDIO_LOOPING
                                        != 0,
                                    fade_out_time: sequence.fade_out_time,
                                },
                            )
                        })
                        .collect(),
                )
            });
            match entry {
                Some(labels) => table.insert_model(&model, labels.clone()),
                None => {}
            }
        }
        if here > 0 {
            maps_with_a_prop += 1;
        }
        server.set_sequences(table);

        spawned += here;
        run(&mut server, RUN_SECONDS);

        for (_, entity) in server.entities.iter() {
            let Some(prop) = entity.behaviour.downcast_ref::<classes::DynamicProp>() else {
                continue;
            };
            props += 1;
            let model = entity.core.model.clone().unwrap_or_default();
            if loaded
                .get(&model.to_ascii_lowercase())
                .map(Option::is_none)
                .unwrap_or(true)
            {
                models_missing += 1;
            }
            if entity.core.effects & movement::EF_NODRAW != 0 {
                invisible += 1;
            }
            match rigidity.get(&model.to_ascii_lowercase()) {
                Some((_, true)) => skinned += 1,
                Some((true, false)) => animatable += 1,
                _ => {}
            }
            match entity.core.solid {
                crate::server::movement::Solid::VPhysics => solid_key[1] += 1,
                _ => solid_key[0] += 1,
            }
            let state = prop.model_state().expect("a prop draws a model");
            if !state.sequence.is_empty() {
                animating += 1;
                match server.sequences.lookup(&model, state.sequence) {
                    sequences::Lookup::Found(_) => resolved += 1,
                    // `Unknown` cannot happen here: a prop that is playing a
                    // sequence has a model, and every model a prop names was
                    // offered to the table above.
                    _ => unresolved += 1,
                }
            }
        }
    }

    let unreadable = loaded.values().filter(|m| m.is_none()).count();
    println!(
        "\n{} maps, {maps_with_a_prop} of them with a prop_dynamic",
        names.len()
    );
    println!(
        "  {spawned} prop_dynamic* spawned, {props} alive after {RUN_SECONDS}s, \
         {} distinct models",
        loaded.len()
    );
    println!("    {unreadable} models would not load; {models_missing} entities wear one");
    for (name, model) in &loaded {
        if model.is_none() {
            println!("      {name}");
        }
    }
    println!("    {invisible} are invisible (StartDisabled, or turned off since)");
    println!(
        "    solid: {} SOLID_NONE-or-OBB, {} SOLID_VPHYSICS",
        solid_key[0], solid_key[1]
    );
    println!(
        "  {animating} are playing a sequence — {resolved} their model has, \
         {unresolved} it does not"
    );
    println!(
        "  {animatable} wear a model with a skeleton; \
         {skinned} wear one that shares a vertex between bones \
         ({} of the {} models)",
        rigidity.values().filter(|(_, shared)| *shared).count(),
        rigidity.len()
    );

    // The census, which is the number every doc in the port quotes.
    assert_eq!(spawned, 8_462, "prop_dynamic + prop_dynamic_override");
    assert_eq!(maps_with_a_prop, 105, "every map but one places a prop");
    assert_eq!(loaded.len(), 606, "distinct models");

    // **Ten props do not survive the first two seconds of their map.** `Kill`
    // (556 shipped connections) and `FadeAndKill` (51) — which is exactly why
    // `ModelEntityState` is keyed rather than positional.
    assert_eq!(props, 8_452, "ten were removed while the map ran");

    // **15 of the 606 models will not read, and 41 entities wear one.**
    //
    // > **All fifteen are `models/props_destruction/toxin*`, and they are the
    // > *flex-delta* refusals** — the ones `studio::every_shipped_studio_model_parses`
    // > counts as "16 non-static models refused", whose `.dx90.vtx` opens with
    // > a strip group this reader does not decode. That reframes them: the
    // > studio port records flex deltas as "absent from the data" on the
    // > grounds that no **static prop** has any, and that is still true — but
    // > `prop_dynamic` is the first thing in the port that *places* one, so
    // > 41 entities across the game now draw nothing where the shipped engine
    // > draws a toxin pipe. That is the second measured condition for reading
    // > flex deltas (the first is skinning's 141 shared-vertex models), and it
    // > is a smaller number than either of `$includemodel`'s.
    assert_eq!(unreadable, 15);
    assert_eq!(models_missing, 41);

    // 1,000 props carry `StartDisabled 1`; the other 127 were switched off by
    // their map's own first two seconds.
    assert_eq!(invisible, 1_127);

    // The `solid` key, which only the prop family writes. 2,830 are
    // `SOLID_NONE` promoted to `SOLID_OBB` (or left alone, for an `_override`)
    // and 5,622 ask for `SOLID_VPHYSICS` and get drawn and walked through,
    // because a `.phy` is `portdocs/ENGINE_TRACE.md` stage 5's.
    assert_eq!(solid_key, [2_830, 5_622]);

    // **What `$includemodel` was worth, measured on the running maps.**
    // Before `studio::include` landed these read 2,563 / 1,666 / **897**: of
    // every sequence still playing after two seconds, more than a third named
    // a label the port could not find, because nine of the 606 models keep
    // their animation in a companion `*_animation.mdl`. Merging those
    // companions leaves **182** unresolved, and that remainder is not a gap —
    // it is Valve's own map errors, 183 `DefaultAnim` keys in the game naming
    // a sequence that is in no model at all (one of the 183 is on a prop that
    // does not survive the two seconds).
    //
    // `animating` rose with it, from 2,563 to 2,738, and that is the second
    // order effect worth naming: an animation that can now *end* fires
    // `OnAnimationDone`, and the game's 5,311 `SetAnimation` connections
    // start more props animating than the map's own `DefaultAnim` keys do.
    assert_eq!(animating, 2_738);
    assert_eq!(resolved, 2_556);
    assert_eq!(unresolved, 182);

    // **What skinning bought**, and before it landed the measured cost of not
    // having it. `prop_dynamic` is what made it concrete: **74 of the 591
    // readable models share a vertex between two bones**, which the per-bone
    // draw split could not express, and **290 entities wear one** — all of
    // them drawn in their bind pose until skinning landed. Seven of those
    // models are on `sp_a1_intro1`
    // (`models/container_ride/finedebris_part*.mdl`), so it was visible on the
    // map this port loads by default rather than only in a census.
    assert_eq!(
        rigidity.values().filter(|(_, shared)| *shared).count(),
        74,
        "models whose vertices are shared between bones"
    );
    assert_eq!(skinned, 290, "entities that needed skinning to pose at all");
    assert_eq!(
        animatable, 3_323,
        "entities whose model the renderer can pose"
    );
}

// ---------------------------------------------------------------------------
// prop_testchamber_door
// ---------------------------------------------------------------------------

/// The one sequence a chamber door plays, as the real `.mdl` describes it:
/// 23 frames at 24 fps, non-looping, with `studiomdl`'s default fade-out.
///
/// Every number here is measured by
/// `studio::tests::the_testchamber_door_model_animates` against the shipped
/// file, so the two cannot drift without one of them failing.
fn chamber_door_sequences() -> sequences::SequenceTable {
    let mut table = sequences::SequenceTable::new();
    table.insert_model(
        "models/props/portal_door_combined.mdl",
        [(
            "open".to_owned(),
            sequences::SequenceInfo {
                duration: 22.0 / 24.0,
                loops: false,
                fade_out_time: 0.2,
            },
        )],
    );
    table
}

/// `open` is 0.9167 seconds long, and `GetLastVisibleCycle` calls it finished
/// 0.2 of those early.
const DOOR_TRAVEL: f32 = 22.0 / 24.0;
const DOOR_LAST_VISIBLE: f32 = DOOR_TRAVEL - 0.2;

/// A map with one chamber door and a counter on each of its four outputs.
fn door_map(extra: &[(&str, &str)]) -> Vec<bsp::Entity> {
    let mut door = block(&[
        ("classname", "prop_testchamber_door"),
        ("targetname", "door"),
        ("origin", "0 0 0"),
    ]);
    for (k, v) in extra {
        door.pairs.push(((*k).to_owned(), (*v).to_owned()));
    }
    for output in ["OnOpen", "OnClose", "OnFullyOpen", "OnFullyClosed"] {
        door.pairs.push((
            output.to_owned(),
            conn(&output.to_ascii_lowercase(), "Add", "1", "0", "-1"),
        ));
    }
    let mut map = vec![block(&[("classname", "worldspawn")]), door];
    for output in ["OnOpen", "OnClose", "OnFullyOpen", "OnFullyClosed"] {
        map.push(block(&[
            ("classname", "math_counter"),
            ("targetname", &output.to_ascii_lowercase()),
        ]));
    }
    map
}

fn door_of(server: &Server) -> &classes::TestChamberDoor {
    find_named(server, "door")
        .behaviour
        .downcast_ref::<classes::TestChamberDoor>()
        .expect("a TestChamberDoor")
}

/// The same by id, for the depot test, which has 138 of them and no name it
/// chose.
fn door_at(server: &Server, id: EntityId) -> &classes::TestChamberDoor {
    server
        .entities
        .get(id)
        .expect("the door")
        .behaviour
        .downcast_ref::<classes::TestChamberDoor>()
        .expect("a TestChamberDoor")
}

/// The five numbers the renderer would be handed for that entity.
fn pose_of(server: &Server, id: EntityId) -> class::ModelState<'_> {
    door_at(server, id).model_state().expect("it draws a model")
}

/// The pose `engine::world::entities` would draw, worked out here from the
/// five numbers the seam carries — the same duplication `prop_cycle` is, and
/// for the same reason.
fn door_cycle(server: &Server) -> f32 {
    let state = door_of(server).model_state().expect("it draws a model");
    let elapsed = (server.time().curtime - state.anim_time).max(0.0);
    (state.cycle + elapsed * state.playback_rate / DOOR_TRAVEL).clamp(0.0, 1.0)
}

fn door_server(extra: &[(&str, &str)]) -> Server {
    let mut server = Server::new();
    server.level_init("test", &door_map(extra), &[]);
    server.set_sequences(chamber_door_sequences());
    server
}

/// **A door spawns shut and still**, on frame zero of `open` at playback rate
/// zero — which is `ResetSequence` followed by `SetPlaybackRate( 0.0f )`, and
/// is the whole reason a chamber does not open itself at map load.
#[test]
fn a_testchamber_door_spawns_shut_and_still() {
    let server = door_server(&[]);
    let door = find_named(&server, "door");

    // `SetModel( TESTCHAMBER_DOOR_MODEL_NAME )` — hard-coded, not a key.
    assert_eq!(
        door.model.as_deref(),
        Some("models/props/portal_door_combined.mdl")
    );
    assert_eq!(door.move_type, crate::server::movement::MoveType::None);
    assert_eq!(door.solid, crate::server::movement::Solid::VPhysics);
    assert!(door.effects & crate::server::keyvalue::effects::NOSHADOW != 0);

    let state = door.behaviour.model_state().expect("it draws a model");
    assert_eq!(state.sequence, "open", "`close` is never played");
    assert_eq!(state.cycle, 0.0);
    assert_eq!(state.playback_rate, 0.0, "shut and held");

    let door = door_of(&server);
    assert!(!door.is_open());
    assert!(!door.is_animating());
    assert!(!door.is_locked());
}

/// **A door opens by playing `open` forwards and shuts by playing it
/// backwards** — 137 and 130 shipped connections — and the pose the renderer
/// would draw follows it both ways.
#[test]
fn a_testchamber_door_opens_forwards_and_shuts_backwards() {
    let mut server = door_server(&[]);
    let id = find_named(&server, "door").id();
    run(&mut server, 0.5);
    assert_eq!(door_cycle(&server), 0.0, "still shut");

    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    assert_eq!(
        door_of(&server)
            .model_state()
            .expect("it draws a model")
            .playback_rate,
        1.0
    );
    assert!(door_of(&server).is_open());
    run(&mut server, 0.1);
    assert_eq!(counter_value(&server, "onopen"), 1.0);

    // Half way through the travel, half way through the sequence — this is
    // the number the renderer computes from the same five fields.
    run(&mut server, DOOR_TRAVEL / 2.0 - 0.1);
    let half = door_cycle(&server);
    assert!(
        (half - 0.5).abs() < 0.02,
        "half a second's travel should be half the sequence: {half}"
    );

    // …and it arrives, and stays arrived.
    run(&mut server, DOOR_TRAVEL);
    assert_eq!(door_cycle(&server), 1.0);
    run(&mut server, 5.0);
    assert_eq!(door_cycle(&server), 1.0, "a non-looping sequence clamps");

    // Now shut it. The rate turns round and the cycle is re-based onto where
    // the door actually is, which is what stops it snapping.
    server.accept_input(id, "Close", Variant::Void, None, None, 0);
    let state = door_of(&server).model_state().expect("it draws a model");
    assert_eq!(state.playback_rate, -1.0);
    assert_eq!(state.cycle, 1.0, "re-based onto the open pose");
    assert!(!door_of(&server).is_open());
    run(&mut server, 0.1);
    assert_eq!(counter_value(&server, "onclose"), 1.0);

    run(&mut server, DOOR_TRAVEL / 2.0 - 0.1);
    let half = door_cycle(&server);
    assert!(
        (half - 0.5).abs() < 0.02,
        "it should be half shut, not snapped: {half}"
    );
    run(&mut server, DOOR_TRAVEL);
    assert_eq!(door_cycle(&server), 0.0, "shut again");
}

/// **Only a door's *first* opening reports its own end**, because
/// `m_bSequenceFinished` is never cleared after `Spawn`.
///
/// The first `OnFullyOpen` waits for the sequence — 0.72 seconds, not 0.92,
/// because `GetLastVisibleCycle` subtracts the 0.2-second fade — and every
/// "fully" output after it fires on the first 10 Hz think following the
/// input, while the door is still visibly moving. It is Valve's, and the 150
/// `OnFullyClosed` connections in the shipped game were authored against it.
#[test]
fn only_a_doors_first_opening_reports_its_own_end() {
    let mut server = door_server(&[]);
    let id = find_named(&server, "door").id();
    run(&mut server, 0.5);

    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    // Not yet: the fade-out threshold is 0.72 seconds in.
    run(&mut server, DOOR_LAST_VISIBLE - 0.15);
    assert_eq!(counter_value(&server, "onfullyopen"), 0.0);
    assert!(door_of(&server).is_animating());

    // …and there, within one think of the threshold.
    run(&mut server, 0.15 + 0.1);
    assert_eq!(counter_value(&server, "onfullyopen"), 1.0);
    assert!(!door_of(&server).is_animating());
    // Once, however long we wait.
    run(&mut server, 3.0);
    assert_eq!(counter_value(&server, "onfullyopen"), 1.0);

    // Now the sticky flag. `Close` starts a 0.92-second travel and
    // `OnFullyClosed` fires on the next think regardless.
    server.accept_input(id, "Close", Variant::Void, None, None, 0);
    run(&mut server, 0.15);
    assert_eq!(
        counter_value(&server, "onfullyclosed"),
        1.0,
        "OnFullyClosed fires a tenth of a second in, not at the end"
    );
    // The door is still most of the way open while that has already happened.
    let cycle = door_cycle(&server);
    assert!(
        cycle > 0.8,
        "the door should still be shutting when OnFullyClosed fires: {cycle}"
    );
    assert!(!door_of(&server).is_animating());

    // The same for a second opening.
    run(&mut server, 2.0);
    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    run(&mut server, 0.15);
    assert_eq!(counter_value(&server, "onfullyopen"), 2.0);
    let cycle = door_cycle(&server);
    assert!(cycle < 0.2, "the door has barely started: {cycle}");
}

/// A door refuses a second `Open` while it is open, and a `Close` while it is
/// shut — so neither fires an output. `IsOpen()` is where the door is
/// *going*, so the refusal starts the instant the input lands.
#[test]
fn a_testchamber_door_refuses_an_input_it_is_already_obeying() {
    let mut server = door_server(&[]);
    let id = find_named(&server, "door").id();
    run(&mut server, 0.5);

    // Shut already.
    server.accept_input(id, "Close", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    assert_eq!(counter_value(&server, "onclose"), 0.0);

    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    // Immediately, while it is still travelling.
    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    assert_eq!(
        counter_value(&server, "onopen"),
        1.0,
        "the second is refused"
    );
}

/// `Lock` refuses both doors until `Unlock`, and `LockOpen` is `Open`
/// *followed* by the lock — so the open itself gets through, which is the
/// point of the input and what its 29 shipped connections want.
#[test]
fn a_locked_testchamber_door_refuses_everything_and_lockopen_gets_in_first() {
    let mut server = door_server(&[]);
    let id = find_named(&server, "door").id();
    run(&mut server, 0.5);

    server.accept_input(id, "Lock", Variant::Void, None, None, 0);
    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    assert_eq!(counter_value(&server, "onopen"), 0.0, "locked shut");
    assert!(door_of(&server).is_locked());

    server.accept_input(id, "Unlock", Variant::Void, None, None, 0);
    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    assert_eq!(counter_value(&server, "onopen"), 1.0);

    // Shut it, then `LockOpen`: the door opens and is locked afterwards.
    server.accept_input(id, "Close", Variant::Void, None, None, 0);
    run(&mut server, 2.0);
    server.accept_input(id, "LockOpen", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    assert_eq!(
        counter_value(&server, "onopen"),
        2.0,
        "the open got through"
    );
    assert!(door_of(&server).is_locked());
    assert!(door_of(&server).is_open());

    // …and now nothing moves it.
    server.accept_input(id, "Close", Variant::Void, None, None, 0);
    run(&mut server, 0.2);
    assert_eq!(
        counter_value(&server, "onclose"),
        1.0,
        "only the earlier one"
    );
}

/// **A door whose model nobody loaded opens, and never finishes opening.**
///
/// `server::sequences`' three-way answer: `Lookup::Unknown` is treated as
/// `LookupSequence` succeeding and `SequenceDuration` returning nothing, so
/// the input is accepted and `OnOpen` fires while `IsSequenceFinished()` can
/// never become true. That is the state every `Spawn` in the game runs in, and
/// the state a headless test without a `Vfs` stays in.
#[test]
fn a_testchamber_door_with_no_model_loaded_opens_but_never_arrives() {
    let mut server = Server::new();
    server.level_init("test", &door_map(&[]), &[]);
    let id = find_named(&server, "door").id();
    run(&mut server, 0.5);

    server.accept_input(id, "Open", Variant::Void, None, None, 0);
    run(&mut server, 5.0);
    assert_eq!(counter_value(&server, "onopen"), 1.0);
    assert_eq!(counter_value(&server, "onfullyopen"), 0.0);
    assert!(door_of(&server).is_animating(), "still owed a `fully`");
}

/// The area portal block is read and kept, not dropped — 84 doors name a
/// window and 94 write the fade triple, and two of them write Hammer
/// instance-fixup leftovers that must read as zero rather than as an error.
#[test]
fn the_area_portal_keys_are_consumed_including_the_two_broken_ones() {
    let server = door_server(&[
        ("AreaPortalWindow", "door_1-door_areaportal_window"),
        ("UseAreaPortalFade", "1"),
        ("AreaPortalFadeStart", "500"),
        ("AreaPortalFadeEnd", "$FadeEndDistance"),
    ]);
    let door = find_named(&server, "door");
    assert!(door.unhandled.is_empty(), "{:?}", door.unhandled);

    let fields = door.behaviour.describe();
    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    };
    assert_eq!(field("area_portal_window"), "door_1-door_areaportal_window");
    assert_eq!(
        field("area_portal_fade"),
        "500..0",
        "the second reads as zero"
    );
}

/// **Every test chamber door in the game, opened and shut, with the real
/// model's sequences in hand.**
///
/// The class's own depot test. For each of the 106 maps it spawns the
/// entities, fills the sequence table from the one `.mdl` every door wears,
/// and then drives each door through a full cycle — checking at every step
/// the things that are a wrong picture rather than an error: that it spawns
/// shut and still, that `Open` turns the sequence forwards, that the pose the
/// renderer would draw arrives at 1.0 and stays, that `Close` re-bases rather
/// than snapping, and that it comes back to 0.0.
///
/// It also measures the sticky-flag behaviour on real data, because it is the
/// one thing about this class that reads as a bug: **a door's first
/// `OnFullyOpen` waits for the sequence and every "fully" output after it
/// does not.**
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release testchamber_door -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_testchamber_door_opens_and_shuts() {
    use crate::filesystem::Vfs;
    use crate::studio::StudioModel;

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

    // The one model every door in the game wears, read once. `Spawn`
    // hard-codes it, so there is exactly one entry in this table and no map
    // can add another.
    const MODEL: &str = "models/props/portal_door_combined.mdl";
    let studio = StudioModel::load(&vfs, MODEL).expect("the door model");
    let mut table = sequences::SequenceTable::new();
    table.insert_model(
        MODEL,
        studio
            .sequences
            .iter()
            .enumerate()
            .map(|(i, sequence)| {
                (
                    sequence.label.clone(),
                    sequences::SequenceInfo {
                        duration: studio.animation(i).map(|a| a.duration()).unwrap_or(0.0),
                        loops: sequence.flags & crate::studio::anim::STUDIO_LOOPING != 0,
                        fade_out_time: sequence.fade_out_time,
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
    let travel = studio
        .animation(studio.sequence("open").expect("`open`"))
        .expect("its animation")
        .duration();

    let mut doors = 0usize;
    let mut maps_with_a_door = 0usize;
    let mut reachable = 0usize;
    // How long each door took to report its own opening, the first time and
    // the second. The first waits for `GetLastVisibleCycle`; the second does
    // not, because `m_bSequenceFinished` is never cleared.
    let (mut first_arrival, mut second_arrival) = (Vec::new(), Vec::new());
    // Doors their own map opens within a second of the level starting.
    let mut opened_by_bootstrap = 0usize;

    for name in &names {
        let bsp = crate::engine::world::bsp::Bsp::load(&vfs, name)
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        let ids: Vec<EntityId> = {
            let mut server = Server::new();
            server.level_init(name, &bsp.entities(), &bsp.models);
            server
                .entities
                .iter()
                .filter(|(_, e)| e.classname() == "prop_testchamber_door")
                .map(|(id, _)| id)
                .collect()
        };
        if ids.is_empty() {
            continue;
        }
        maps_with_a_door += 1;
        doors += ids.len();

        // Reachability: does anything in the map fire `Open` at this door?
        // The shipped answer is what says the class is worth having.
        let names_opened: Vec<String> = bsp
            .entities()
            .iter()
            .flat_map(|e| e.pairs.iter())
            .filter_map(|(_, v)| {
                let mut parts = v.split('\u{1b}');
                let target = parts.next()?.to_ascii_lowercase();
                let input = parts.next()?;
                (input.eq_ignore_ascii_case("Open") || input.eq_ignore_ascii_case("LockOpen"))
                    .then_some(target)
            })
            .collect();

        // One door at a time, from a fresh level each time, so that a map's
        // own bootstrap cannot have moved the others.
        for &id in &ids {
            let mut server = Server::new();
            server.level_init(name, &bsp.entities(), &bsp.models);
            server.set_sequences(table.clone());

            let door_name = server
                .entities
                .get(id)
                .and_then(|e| e.core.name.clone())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if names_opened.iter().any(|t| *t == door_name) {
                reachable += 1;
            }

            let rate = |server: &Server| pose_of(server, id).playback_rate;
            let cycle = |server: &Server| {
                let s = pose_of(server, id);
                let elapsed = (server.time().curtime - s.anim_time).max(0.0);
                (s.cycle + elapsed * s.playback_rate / travel).clamp(0.0, 1.0)
            };

            // A second of the map's own bootstrap. Almost every door in the
            // game is still shut and still after it — a chamber waits for the
            // player — and the handful that are not are counted rather than
            // asserted away.
            run(&mut server, 1.0);
            assert_eq!(pose_of(&server, id).sequence, "open", "{name}: {door_name}");
            if door_at(&server, id).is_open() {
                opened_by_bootstrap += 1;
                continue;
            }
            assert_eq!(rate(&server), 0.0, "{name}: {door_name}");
            assert_eq!(cycle(&server), 0.0, "{name}: {door_name} is not shut");

            // Open it, and time how long it takes to say it has.
            let started = server.time().curtime;
            server.accept_input(id, "Open", Variant::Void, None, None, 0);
            assert_eq!(rate(&server), 1.0, "{name}: {door_name}");
            assert!(door_at(&server, id).is_open(), "{name}: {door_name}");
            let mut arrival = None;
            for _ in 0..40 {
                run(&mut server, 0.05);
                if !door_at(&server, id).is_animating() && arrival.is_none() {
                    arrival = Some(server.time().curtime - started);
                }
            }
            first_arrival.push(arrival.expect("the door reports opening"));
            assert_eq!(cycle(&server), 1.0, "{name}: {door_name} did not arrive");

            // Shut it. The cycle must come *down* from 1 rather than snap.
            server.accept_input(id, "Close", Variant::Void, None, None, 0);
            assert_eq!(rate(&server), -1.0, "{name}: {door_name}");
            assert_eq!(
                pose_of(&server, id).cycle,
                1.0,
                "{name}: {door_name} snapped"
            );
            run(&mut server, travel / 2.0);
            let half = cycle(&server);
            assert!(
                (half - 0.5).abs() < 0.05,
                "{name}: {door_name} is at {half} half way through shutting"
            );
            run(&mut server, travel);
            assert_eq!(cycle(&server), 0.0, "{name}: {door_name} did not shut");

            // …and open it again, which is the sticky flag.
            let started = server.time().curtime;
            server.accept_input(id, "Open", Variant::Void, None, None, 0);
            let mut arrival = None;
            for _ in 0..40 {
                run(&mut server, 0.05);
                if !door_at(&server, id).is_animating() && arrival.is_none() {
                    arrival = Some(server.time().curtime - started);
                }
            }
            second_arrival.push(arrival.expect("the door reports opening"));
        }
    }

    let range = |v: &[f32]| {
        let lo = v.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (lo, hi)
    };
    let (first_lo, first_hi) = range(&first_arrival);
    let (second_lo, second_hi) = range(&second_arrival);
    println!(
        "{doors} prop_testchamber_door across {maps_with_a_door} of {} maps, \
         {reachable} of them opened by their map\n  \
         {opened_by_bootstrap} opened by their own map's bootstrap\n  \
         `open` is {travel:.4}s; first OnFullyOpen after {first_lo:.3}..{first_hi:.3}s, \
         second after {second_lo:.3}..{second_hi:.3}s",
        names.len()
    );

    assert_eq!(doors, 138);
    assert_eq!(maps_with_a_door, 71);
    // **130 of the 138 are opened by something in their own map**, which is
    // what says the class is worth having: the chain that gets there —
    // `trigger_once` → `func_instance_io_proxy` → `logic_relay` → the door —
    // is ported end to end. Of the other eight, **five carry no `targetname`
    // at all** so nothing can aim at one, and three are named and never
    // fired at (`sp_a1_intro3`'s `door_2`, `sp_a2_column_blocker`'s
    // `testchamber_door` and `sp_a4_laser_platform`'s `tb_catch_inner_door`)
    // — Valve's own dead map data, and all eight stand shut in the shipped
    // game too.
    assert_eq!(reachable, 130);
    // Seven doors are opened by their own map within a second of the level
    // starting, so "a chamber door waits for the player" is nearly but not
    // quite a rule.
    assert_eq!(opened_by_bootstrap, 7);

    // **The sticky flag, measured.** The first opening waits for
    // `GetLastVisibleCycle` — 0.9167 - 0.2 = 0.717 seconds — and is noticed on
    // the next 10 Hz think, which `SetNextThink` has quantised to six 64 Hz
    // ticks. The second is noticed on the first think after the input,
    // because `m_bSequenceFinished` was never cleared.
    // Every one of the 131 driven doors reports the same two times, because
    // the whole schedule is quantised: 0.797s the first time, against a
    // 0.9167s travel, and 0.094s — one think — the second.
    assert_eq!((first_lo, first_hi), (0.796875, 0.796875));
    assert_eq!((second_lo, second_hi), (0.09375, 0.09375));
}

/// **`sp_a1_intro1`'s door, opened the way the map opens it** — through the
/// chain the mapper actually built rather than by firing `Open` at it.
///
/// The default map's two doors are driven by
///
/// ```text
///   trigger_once → logic_relay "door_open_relay"
///                → func_instance_io_proxy "door_1-proxy" (OnProxyRelay2)
///                → logic_relay "door_1-door_open_relay"
///                → prop_testchamber_door "door_1-testchamber_door" (Open)
/// ```
///
/// and every class in it is ported, which is what makes the door open in the
/// running game when you walk into the chamber. This test fires `Trigger` at
/// the head of the chain and watches the far end of it, so a break anywhere
/// in between shows up here rather than as a door that silently never moves.
///
/// **And it shuts again**, which is a longer chain and one the map does not
/// build out of relays alone:
///
/// ```text
///   trigger_once → logic_relay "door_close_relay"
///                → func_instance_io_proxy "door_1-proxy" (OnProxyRelay1)
///                → logic_relay "door_1-door_close_relay"
///                → logic_branch "door_1-door_wants_to_close_branch" (SetValue 1)
///                ⇢ logic_branch_listener "door_1-door_can_close_branch_listener"
///                → logic_relay "door_1-close_door_rl"
///                → prop_testchamber_door "door_1-testchamber_door" (Close)
///                → logic_branch "door_1-door_wants_to_close_branch" (SetValue 0)
/// ```
///
/// The last line is the same branch again: the relay that shuts the door
/// consumes the flag that asked for it, so by the end of the tick the branch
/// reads `false` and the listener reads `mixed` — which looks exactly like a
/// chain that never arrived, and is why this test checks the **door** and not
/// the branch.
///
/// The `⇢` is the only hop in either chain that is not a connection a mapper
/// typed: it is the branch posting `_OnLogicBranchChanged` at the listener
/// that registered with it. The listener's other branch,
/// `door_1-player_not_in_door_branch`, starts `1` and is driven to `0` by a
/// `trigger_multiple` in the doorway — so with nobody standing in the door the
/// verdict flips to all-true the instant the map asks, and the door closes.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_intro_maps_door -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn the_intro_maps_door_opens_through_the_chain_its_map_built() {
    use crate::filesystem::Vfs;

    let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
        panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
    };
    let dir = std::path::PathBuf::from(dir);
    let base = dir.parent().unwrap_or(&dir).to_path_buf();
    let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");
    let bsp = crate::engine::world::bsp::Bsp::load(&vfs, "sp_a1_intro1").expect("the intro map");

    let mut server = Server::new();
    server.level_init("sp_a1_intro1", &bsp.entities(), &bsp.models);
    run(&mut server, 1.0);

    let door = find_named(&server, "door_1-testchamber_door").id();
    assert!(!door_at(&server, door).is_open(), "it starts shut");
    assert_eq!(pose_of(&server, door).playback_rate, 0.0);

    // The head of the chain — what the `trigger_once` at the chamber entrance
    // fires when the player walks through it.
    let head = find_named(&server, "door_open_relay").id();
    server.accept_input(head, "Trigger", Variant::Void, None, None, 0);

    // Zero-delay all the way, so the whole chain lands inside one tick: the
    // queue restarts from the head after every event.
    run(&mut server, 1.0 / 64.0);
    assert!(
        door_at(&server, door).is_open(),
        "the map's own chain did not reach the door"
    );
    assert_eq!(pose_of(&server, door).playback_rate, 1.0, "it is opening");

    // …and the second door, whose chain is one link shorter.
    let exit = find_named(&server, "@exit_door-testchamber_door").id();
    let head = find_named(&server, "@exit_door-door_open_relay").id();
    assert!(!door_at(&server, exit).is_open());
    server.accept_input(head, "Trigger", Variant::Void, None, None, 0);
    run(&mut server, 1.0 / 64.0);
    assert!(door_at(&server, exit).is_open());

    // -----------------------------------------------------------------------
    // and shut again, through the `logic_branch_listener` half
    // -----------------------------------------------------------------------

    // Let the first door finish opening, so that what follows is a real
    // reversal rather than a rate flip mid-travel.
    run(&mut server, 1.0);

    let wants = find_named(&server, "door_1-door_wants_to_close_branch").id();
    let clear = find_named(&server, "door_1-player_not_in_door_branch").id();
    let listener = find_named(&server, "door_1-door_can_close_branch_listener").id();
    assert_eq!(branch_list_at(&server, listener).branches().len(), 2);
    assert_eq!(
        branch_list_at(&server, listener).state(),
        "not-init",
        "it has reported nothing at all yet"
    );
    assert!(
        !branch_value(&server, wants),
        "nothing has asked for it to shut yet"
    );
    assert!(
        branch_value(&server, clear),
        "`InitialValue 1` — nobody is standing in the doorway"
    );

    // The head of the *close* chain: the `trigger_once` beyond the door.
    let head = find_named(&server, "door_close_relay").id();
    server.accept_input(head, "Trigger", Variant::Void, None, None, 0);

    // Zero delay the whole way, branch notification included, so all of it
    // lands inside a single tick.
    run(&mut server, 1.0 / 64.0);
    assert!(
        !door_at(&server, door).is_open(),
        "the branch listener did not reach the door"
    );

    // **And the branch is back to `false`**, which is not a failure to arrive:
    // `door_1-close_door_rl` fires `SetValue 0` at it in the same breath as
    // the door's `Close`, so the condition is consumed. The listener's own
    // verdict followed it back down to mixed.
    assert!(
        !branch_value(&server, wants),
        "the chain consumed its own flag"
    );
    assert_eq!(
        branch_list_at(&server, listener).state(),
        "mixed",
        "all-true, then talked straight back down"
    );
    assert_eq!(
        pose_of(&server, door).playback_rate,
        -1.0,
        "`open` is played backwards to shut"
    );

    println!(
        "sp_a1_intro1: both chamber doors open through their own map logic, \
         and door_1 shuts again through its logic_branch_listener"
    );
}

// ---------------------------------------------------------------------------
// prop_portal — `portdocs/PORTAL.md` stage 2
// ---------------------------------------------------------------------------

use crate::server::classes::portal::{self, PropPortal};

/// A `prop_portal` block, as the shipped maps write one.
fn portal_block(name: &str, origin: &str, angles: &str, portal_two: &str) -> bsp::Entity {
    block(&[
        ("classname", "prop_portal"),
        ("targetname", name),
        ("origin", origin),
        ("angles", angles),
        ("Activated", "0"),
        ("PortalTwo", portal_two),
    ])
}

fn portal_at<'a>(server: &'a Server, name: &str) -> &'a PropPortal {
    find_named(server, name)
        .behaviour
        .downcast_ref::<PropPortal>()
        .expect("a PropPortal")
}

/// Switches a portal on or off the way a map does — through the input, so that
/// the whole `SetActive` / `UpdatePortalLinkage` path runs.
fn set_activated(server: &mut Server, name: &str, on: bool) {
    let id = find_named(server, name).id();
    server.accept_input(id, "SetActivatedState", Variant::Bool(on), None, None, 0);
}

/// `sp_a1_intro1`'s two portals, verbatim from its entity lump.
fn intro1_portals() -> Vec<bsp::Entity> {
    vec![
        portal_block("portal_blue_0", "-1264 4112 2728", "0 90 0", "0"),
        portal_block("portal_red_0", "-1137 4352 2762", "0 180 0", "1"),
    ]
}

/// Spawn: the model name, the one-sided trigger box, and the default size —
/// which is **56 half-height and not 14**, the number the reference tree's own
/// file-scope initializer gets deliberately wrong.
#[test]
fn a_portal_spawns_as_a_one_sided_box_trigger() {
    let mut server = Server::new();
    server.level_init("test", &intro1_portals(), &[]);

    let entity = find_named(&server, "portal_blue_0");
    assert_eq!(entity.core.solid, movement::Solid::Obb);
    assert!(entity.core.is_solid_flag_set(movement::FSOLID_TRIGGER));
    assert!(entity.core.is_solid_flag_set(movement::FSOLID_NOT_SOLID));
    assert!(!entity.core.is_solid(), "a portal is not a wall");
    assert_eq!(entity.core.move_type, movement::MoveType::None);
    assert_eq!(entity.core.model.as_deref(), Some(portal::MODEL_PORTAL_1));

    // `GetLocalMins()` .. `GetLocalMaxs()`: nothing behind the plane, 64 units
    // in front of it.
    assert_eq!(
        entity.core.model_bounds.mins,
        Vec3::new(0.0, -32.0, -56.0),
        "the box starts at the portal plane"
    );
    assert_eq!(
        entity.core.model_bounds.maxs,
        Vec3::new(portal::OBB_DEPTH, 32.0, 56.0)
    );

    let blue = portal_at(&server, "portal_blue_0");
    assert!(!blue.activated, "all 21 shipped portals start switched off");
    assert!(blue.linked.is_none());
    assert_eq!(
        blue.matrix,
        glam::Mat4::IDENTITY,
        "no partner, no transform"
    );
    assert_eq!(blue.linkage_group, 0);
    assert_eq!(blue.half_width, portal::DEFAULT_HALF_WIDTH);
    assert_eq!(blue.half_height, portal::DEFAULT_HALF_HEIGHT);

    // The second model, from the second colour.
    assert_eq!(
        find_named(&server, "portal_red_0").core.model.as_deref(),
        Some(portal::MODEL_PORTAL_2)
    );

    // **The model is never drawn**, which is the whole of `writez` — see
    // `PropPortal::model_state`. Nothing in the seam, so nothing for
    // `world/entities` to resolve into a magenta checkerboard.
    assert!(
        server.model_entities().is_empty(),
        "a portal's model is depth-only and must not reach the renderer"
    );
}

/// Two portals link when both are on, and **the colour is an output of
/// linking** rather than an input to it.
#[test]
fn two_active_portals_in_a_group_find_each_other() {
    let mut server = Server::new();
    server.level_init("test", &intro1_portals(), &[]);

    set_activated(&mut server, "portal_blue_0", true);
    assert!(
        portal_at(&server, "portal_blue_0").linked.is_none(),
        "one active portal has nobody to link to"
    );

    set_activated(&mut server, "portal_red_0", true);
    let blue = find_named(&server, "portal_blue_0").id();
    let red = find_named(&server, "portal_red_0").id();
    assert_eq!(portal_at(&server, "portal_blue_0").linked, Some(red));
    assert_eq!(portal_at(&server, "portal_red_0").linked, Some(blue));
    assert!(portal_at(&server, "portal_blue_0").is_active_and_linked());

    // They were already opposite, so nothing moved — but the colours must
    // still *be* opposite, which is the invariant the forcing line keeps.
    assert!(!portal_at(&server, "portal_blue_0").is_portal2);
    assert!(portal_at(&server, "portal_red_0").is_portal2);
}

/// `m_bIsPortal2 = !m_hLinkedPortal->m_bIsPortal2` — and it runs on the
/// partner first, so **the portal that activates second keeps its colour**.
#[test]
fn linking_two_portals_of_one_colour_flips_the_first() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            portal_block("first", "0 0 0", "0 0 0", "0"),
            portal_block("second", "256 0 0", "0 180 0", "0"),
        ],
        &[],
    );

    set_activated(&mut server, "first", true);
    set_activated(&mut server, "second", true);

    assert!(
        portal_at(&server, "first").is_portal2,
        "the first was turned orange by the second"
    );
    assert!(
        !portal_at(&server, "second").is_portal2,
        "the second keeps the colour its map gave it"
    );
}

/// Deactivating unlinks both sides, and the partner **looks for somebody
/// else** — the one real recursion in `UpdatePortalLinkage`, which needs three
/// portals to be visible at all.
#[test]
fn deactivating_a_portal_hands_its_partner_on() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &[
            portal_block("a", "0 0 0", "0 0 0", "0"),
            portal_block("b", "256 0 0", "0 180 0", "1"),
            portal_block("c", "0 256 0", "0 270 0", "1"),
        ],
        &[],
    );

    set_activated(&mut server, "a", true);
    set_activated(&mut server, "b", true);
    let (a, b, c) = (
        find_named(&server, "a").id(),
        find_named(&server, "b").id(),
        find_named(&server, "c").id(),
    );
    assert_eq!(portal_at(&server, "a").linked, Some(b));

    // A third portal switching on finds nobody: both of the others are taken.
    set_activated(&mut server, "c", true);
    assert_eq!(portal_at(&server, "c").linked, None);
    assert_eq!(portal_at(&server, "a").linked, Some(b));

    // Now switch `b` off. `a` is orphaned and must be handed to `c`.
    set_activated(&mut server, "b", false);
    assert_eq!(portal_at(&server, "b").linked, None);
    assert_eq!(portal_at(&server, "a").linked, Some(c));
    assert_eq!(portal_at(&server, "c").linked, Some(a));
    assert_ne!(
        portal_at(&server, "a").matrix,
        glam::Mat4::IDENTITY,
        "the new pair's matrices were recomputed"
    );
    assert_eq!(
        portal_at(&server, "b").matrix,
        glam::Mat4::IDENTITY,
        "a portal with no partner teleports nothing"
    );
}

/// The teleport matrix, against the two things it must do.
///
/// It is `portdocs/PORTAL.md` §11's first test: a point through the matrix and
/// back through the inverse is itself, and a portal linked to a copy of itself
/// at the same place turns a point around.
#[test]
fn the_teleport_matrix_turns_a_point_around_the_exit() {
    use crate::server::classes::portal::teleport_matrix;

    let entrance = (
        Vec3::new(-1264.0, 4112.0, 2728.0),
        Vec3::new(0.0, 90.0, 0.0),
    );
    let exit = (
        Vec3::new(-1137.0, 4352.0, 2762.0),
        Vec3::new(0.0, 180.0, 0.0),
    );
    let there = teleport_matrix(entrance, exit);
    let back = teleport_matrix(exit, entrance);

    // A round trip is the identity: the two matrices are each other's inverse,
    // which is what `UTIL_Portal_ComputeMatrix` computing both ends buys.
    let point = Vec3::new(-1200.0, 4200.0, 2740.0);
    let round = back.transform_point3(there.transform_point3(point));
    assert!((round - point).length() < 1e-2, "{round} vs {point}");

    // **A point just behind the entrance comes out just in front of the
    // exit**, which is the whole teleport. The entrance faces `+Y` and the
    // exit `-X`.
    let entrance_forward = Vec3::Y;
    let exit_forward = -Vec3::X;
    let crossing = entrance.0 - entrance_forward * 4.0;
    let arrived = there.transform_point3(crossing);
    assert!(
        (arrived - exit.0).dot(exit_forward) > 3.9,
        "{arrived} is not in front of the exit"
    );

    // …and a velocity *into* the entrance comes out *out of* the exit, which
    // is the 180° turn about up. Leave it out and the player backs into the
    // wall behind the exit.
    let velocity = -entrance_forward * 200.0;
    let exited = there.transform_vector3(velocity);
    assert!(
        (exited - exit_forward * 200.0).length() < 1e-2,
        "{exited} should be 200 along {exit_forward}"
    );

    // A portal linked to a copy of itself, in the same place: the transform is
    // the half turn about its own up and nothing else.
    let self_linked = teleport_matrix(entrance, entrance);
    let up = Vec3::Z;
    let right = Vec3::X;
    let offset = |v: Vec3| self_linked.transform_point3(entrance.0 + v) - entrance.0;
    assert!(
        (offset(up * 10.0) - up * 10.0).length() < 1e-2,
        "up survives"
    );
    assert!(
        (offset(right * 10.0) + right * 10.0).length() < 1e-2,
        "right reverses"
    );
    assert!(
        (offset(entrance_forward * 10.0) + entrance_forward * 10.0).length() < 1e-2,
        "forward reverses"
    );
}

/// `NewLocation` moves a portal **and switches it on**, which is the side
/// effect that makes the console command one call rather than two.
#[test]
fn new_location_moves_a_portal_and_activates_it() {
    let mut server = Server::new();
    server.level_init("test", &intro1_portals(), &[]);

    let id = find_named(&server, "portal_blue_0").id();
    server.accept_input(
        id,
        "NewLocation",
        Variant::String("64 128 256 0 45 0".to_owned()),
        None,
        None,
        0,
    );

    let entity = find_named(&server, "portal_blue_0");
    assert_eq!(entity.core.origin, Vec3::new(64.0, 128.0, 256.0));
    assert_eq!(entity.core.angles, Vec3::new(0.0, 45.0, 0.0));
    assert!(
        portal_at(&server, "portal_blue_0").activated,
        "`SetActive( true )` is in the middle of the base class's NewLocation"
    );
}

/// `Resize` unlinks a pair that no longer matches, because the partner search
/// compares both half-extents exactly.
#[test]
fn resizing_one_portal_of_a_pair_unlinks_it() {
    let mut server = Server::new();
    server.level_init("test", &intro1_portals(), &[]);
    set_activated(&mut server, "portal_blue_0", true);
    set_activated(&mut server, "portal_red_0", true);
    assert!(portal_at(&server, "portal_blue_0").linked.is_some());

    let id = find_named(&server, "portal_blue_0").id();
    server.accept_input(
        id,
        "Resize",
        Variant::String("16 28".to_owned()),
        None,
        None,
        0,
    );

    let blue = portal_at(&server, "portal_blue_0");
    assert_eq!(blue.half_width, 16.0);
    assert_eq!(blue.half_height, 28.0);
    assert!(blue.linked.is_none(), "different sizes cannot link");
    assert!(portal_at(&server, "portal_red_0").linked.is_none());
}

/// The `portal` console command's server half: two calls make a linked pair,
/// out of nothing, on a map that places no `prop_portal` at all.
#[test]
fn placing_a_pair_by_hand_creates_and_links_two_portals() {
    let mut server = Server::new();
    server.level_init("test", &[block(&[("classname", "info_target")])], &[]);
    assert!(server.portals().is_empty());

    assert!(server.place_portal(false, Vec3::new(0.0, 0.0, 64.0), Vec3::ZERO));
    let portals = server.portals();
    assert_eq!(portals.len(), 1, "one active portal");
    assert!(portals[0].linked.is_none(), "nobody to link to yet");
    assert_eq!(portals[0].half_height, portal::DEFAULT_HALF_HEIGHT);

    assert!(server.place_portal(
        true,
        Vec3::new(256.0, 0.0, 64.0),
        Vec3::new(0.0, 180.0, 0.0)
    ));
    let portals = server.portals();
    assert_eq!(portals.len(), 2);
    assert!(
        portals.iter().all(|p| p.linked.is_some()),
        "a pair, both ways"
    );
    assert_eq!(
        portals.iter().filter(|p| p.is_portal2).count(),
        1,
        "one of each colour"
    );

    // Placing the same colour again moves the portal that is already there
    // rather than making a third — `FindPortal` prefers an active match.
    assert!(server.place_portal(false, Vec3::new(0.0, 128.0, 64.0), Vec3::ZERO));
    assert_eq!(server.portals().len(), 2, "moved, not multiplied");

    assert_eq!(server.fizzle_portals(), 2);
    assert!(
        server.portals().is_empty(),
        "an inactive portal is not drawn"
    );
    assert_eq!(server.fizzle_portals(), 0, "nothing left to fizzle");
}

/// **Every one of the game's 21 `prop_portal`s spawns, and every map that
/// places a pair produces exactly one linked pair once its own
/// `SetActivatedState 1` connections have been fired.**
///
/// `portdocs/PORTAL.md` §11's fourth test. It does not wait for the map's own
/// logic to reach the portals — most of the 31 connections are minutes into a
/// chamber — so it fires each connection's input by hand and then checks the
/// linkage the class produced.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_portal -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_portal_spawns_and_its_map_can_link_a_pair() {
    use crate::engine::world::bsp::Bsp;
    use crate::filesystem::Vfs;

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

    let (mut maps, mut spawned, mut activated, mut linked_maps) = (0, 0, 0, 0);
    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let entities = bsp.entities();
        let count = entities
            .iter()
            .filter(|e| e.classname() == Some("prop_portal"))
            .count();
        if count == 0 {
            continue;
        }
        maps += 1;

        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);

        let ids: Vec<EntityId> = server
            .entities
            .iter()
            .filter(|(_, e)| e.behaviour.downcast_ref::<PropPortal>().is_some())
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids.len(),
            count,
            "{name}: {count} placed, {} spawned",
            ids.len()
        );
        spawned += ids.len();
        for &id in &ids {
            let portal = server
                .entities
                .get(id)
                .expect("live")
                .behaviour
                .downcast_ref::<PropPortal>()
                .expect("a PropPortal");
            assert!(!portal.activated, "{name}: every shipped portal starts off");
            assert_eq!(portal.linkage_group, 0, "{name}: no map writes the key");
        }

        // Switch on whatever the map's own logic would switch on. A portal
        // with no `SetActivatedState 1` aimed at it stays off, which is what
        // `sp_a4_finale1`'s `NewLocation` pair does instead.
        //
        // Every value in the lump is scanned for a connection, rather than
        // only the keys that look like output names: `AddOutput` and the
        // instance proxies mean an output can be spelled anything, and a
        // connection is recognisable by its own shape — five fields separated
        // by `\x1b`.
        let fires_on = |target: &str| {
            entities.iter().any(|block| {
                block.pairs.iter().any(|(_, value)| {
                    let mut fields = value.split('\u{1b}');
                    let to = fields.next();
                    let input = fields.next();
                    let parameter = fields.next();
                    fields.count() == 2
                        && to.is_some_and(|to| to.eq_ignore_ascii_case(target))
                        && input.is_some_and(|i| i.eq_ignore_ascii_case("SetActivatedState"))
                        && parameter.is_some_and(|p| p.trim() == "1")
                })
            })
        };
        let wanted: Vec<EntityId> = ids
            .iter()
            .copied()
            .filter(|&id| {
                server
                    .entities
                    .get(id)
                    .and_then(|e| e.core.name.clone())
                    .is_some_and(|name| fires_on(&name))
            })
            .collect();
        for &id in &wanted {
            server.accept_input(id, "SetActivatedState", Variant::Bool(true), None, None, 0);
        }
        activated += server.portals().len();

        // **The linkage is well formed**, which is the property with teeth and
        // is not "exactly one pair": `sp_a1_intro2` places *four* portals, and
        // firing every `SetActivatedState 1` in the lump at once — which the
        // running map never does, because its portals belong to different
        // rooms — legitimately forms two pairs. So what is asserted is the
        // invariant: every linked portal's partner links back to it, and no
        // portal is claimed twice.
        let mut partners: Vec<(EntityId, EntityId)> = Vec::new();
        for &id in &ids {
            let Some(entity) = server.entities.get(id) else {
                continue;
            };
            let portal = entity
                .behaviour
                .downcast_ref::<PropPortal>()
                .expect("a PropPortal");
            let Some(partner) = portal.linked else {
                continue;
            };
            assert!(portal.activated, "{name}: an inactive portal kept a link");
            let back = server
                .entities
                .get(partner)
                .and_then(|e| e.behaviour.downcast_ref::<PropPortal>())
                .and_then(|p| p.linked);
            assert_eq!(back, Some(id), "{name}: the link is one-way");
            assert_ne!(
                portal.is_portal2,
                server
                    .entities
                    .get(partner)
                    .and_then(|e| e.behaviour.downcast_ref::<PropPortal>())
                    .expect("a partner")
                    .is_portal2,
                "{name}: a linked pair must be two colours"
            );
            assert_ne!(
                portal.matrix,
                glam::Mat4::IDENTITY,
                "{name}: a linked portal has a real transform"
            );
            partners.push((id, partner));
        }
        let linked = partners.len();
        let mut claimed: Vec<EntityId> = partners.iter().map(|&(_, to)| to).collect();
        claimed.sort_by_key(|id| id.to_int());
        let before = claimed.len();
        claimed.dedup();
        assert_eq!(before, claimed.len(), "{name}: a portal was claimed twice");

        println!(
            "{name}: {} portals, {} switched on by its own logic, {linked} linked ({} pairs)",
            ids.len(),
            server.portals().len(),
            linked / 2,
        );
        if linked > 0 {
            linked_maps += 1;
        }
    }

    println!(
        "{spawned} prop_portals across {maps} maps; {activated} activate from map logic, \
         {linked_maps} maps form a pair"
    );
    assert_eq!(maps, 10, "ten maps place a portal");
    assert_eq!(spawned, 21, "twenty-one portals in the game");
    assert!(linked_maps > 0, "no map managed to link a pair");
}

/// **The placement snap would never turn a shipped portal, and would move only
/// four of the twenty-one** — which is what makes skipping it a divergence with
/// a bounded consequence rather than an unknown one.
///
/// `portdocs/PORTAL.md` §11's fifth test, and it justifies a *deletion* rather
/// than checking an implementation. `CProp_Portal::ActivatePortal`
/// (`prop_portal.cpp:700`) traces **one unit in front of the portal to eight
/// units behind it**, takes the surface normal as the portal's new forward and
/// re-places itself at the hit point. This port activates a portal where the
/// map put it, so what has to be true is that the trace would not have changed
/// anything that matters.
///
/// The three outcomes, and all three are the map's doing rather than the
/// port's:
///
/// - **Flush** — a surface within a unit, parallel to the portal. The snap is a
///   no-op.
/// - **Proud** — a surface further back than a unit but still parallel, because
///   the mapper placed the portal standing off its wall. The snap would pull it
///   in; nothing about the portal's frame changes, so a teleport through it
///   lands in the same place facing the same way.
/// - **Floating** — no surface within nine units at all, which is every one of
///   the **four `NewLocation` targets**: `sp_a4_finale1` and `sp_a4_finale2`
///   each park two tractor-beam portals in mid-air at spawn and move them onto
///   a wall from script. §11 names these as the interesting exceptions and they
///   are exactly that — and it is also why neither map switches a portal on
///   with `SetActivatedState`.
///
/// The assertion with teeth is the **angle**: not one of the 21 is more than a
/// degree off the surface behind it, so the snap cannot re-orient a shipped
/// portal, and the teleport matrix a map's own placement produces is the one
/// the shipped game computes.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_portal_is_on_a_wall -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_portal_is_on_a_wall() {
    use crate::engine::trace::{CollisionBsp, Contents, Ray};
    use crate::engine::world::bsp::Bsp;

    /// `UTIL_TraceLine( vOrigin + vForward, vOrigin + vForward * -8.0f, ... )`.
    const AHEAD: f32 = 1.0;
    const BEHIND: f32 = -8.0;
    /// How far the snap may move a portal before it counts as *proud* rather
    /// than flush, and how far it may turn one before the divergence stops
    /// being free.
    const FLUSH: f32 = 1.0;
    const DEGREES: f32 = 1.0;

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

    let (mut checked, mut flush, mut proud, mut floating) = (0, 0, 0, 0);
    let (mut worst_distance, mut worst_angle) = (0.0f32, 0.0f32);
    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let entities = bsp.entities();
        if !entities
            .iter()
            .any(|e| e.classname() == Some("prop_portal"))
        {
            continue;
        }
        let collision = CollisionBsp::build(&bsp);

        // Which portals the map moves from script rather than placing. All four
        // are expected to be floating, and nothing else is.
        let is_a_new_location_target = |target: &str| {
            entities.iter().any(|block| {
                block.pairs.iter().any(|(_, value)| {
                    let mut fields = value.split('\u{1b}');
                    let to = fields.next();
                    let input = fields.next();
                    fields.count() == 3
                        && to.is_some_and(|to| to.eq_ignore_ascii_case(target))
                        && input.is_some_and(|i| i.eq_ignore_ascii_case("NewLocation"))
                })
            })
        };

        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);
        let ids: Vec<EntityId> = server
            .entities
            .iter()
            .filter(|(_, e)| e.behaviour.downcast_ref::<PropPortal>().is_some())
            .map(|(id, _)| id)
            .collect();

        for id in ids {
            let core = &server.entities.get(id).expect("live").core;
            let (origin, forward) = (core.origin, PropPortal::forward(core));
            let label = core.debug_name().to_owned();
            let moved_by_script = core.name.as_deref().is_some_and(is_a_new_location_target);
            checked += 1;

            let ray = Ray::line(origin + forward * AHEAD, origin + forward * BEHIND);
            let hit = collision.tracer().trace(&ray, Contents::MASK_SHOT_PORTAL);
            if !hit.did_hit() {
                floating += 1;
                println!("  {name}/{label}: floating, moved by NewLocation: {moved_by_script}");
                assert!(
                    moved_by_script,
                    "{name}/{label}: nothing behind it and no script to move it"
                );
                continue;
            }

            // How far the snap would have moved it, and how far it would have
            // turned it.
            let distance = (hit.end - origin).length();
            let angle = forward.dot(hit.normal).clamp(-1.0, 1.0).acos().to_degrees();
            worst_distance = worst_distance.max(distance);
            worst_angle = worst_angle.max(angle);
            assert!(
                angle <= DEGREES,
                "{name}/{label}: the snap would turn it {angle:.2} degrees, \
                 so this port's teleport matrix is not the shipped game's"
            );
            match distance <= FLUSH {
                true => flush += 1,
                false => {
                    proud += 1;
                    println!("  {name}/{label}: proud of its wall by {distance:.3} units");
                }
            }
        }
    }

    println!(
        "{checked} shipped portals: {flush} flush, {proud} proud of their wall, \
         {floating} floating (moved by NewLocation); \
         the snap would move one at most {worst_distance:.3} units \
         and turn one at most {worst_angle:.2} degrees"
    );
    assert_eq!(checked, 21, "twenty-one portals in the game");
    assert_eq!(
        floating, 4,
        "the four NewLocation targets, and nothing else"
    );
    assert!(
        worst_angle <= DEGREES,
        "a portal the snap would have re-oriented"
    );
}

// ---------------------------------------------------------------------------
// `portdocs/PORTAL.md` stage 5 — the punch, and the two animation clocks
// ---------------------------------------------------------------------------

/// A [`TouchQuery`] that answers one fixed thing to
/// [`start_solid`](TouchQuery::start_solid) and nothing to the other half.
///
/// The punch's second condition is *"the world says the player is embedded"*,
/// and the world is the engine's; a `server/` test that had to build a `.bsp`
/// to say "yes" would be testing the collision model rather than the guard.
struct Embedded(bool);

impl TouchQuery for Embedded {
    fn brush_models_touching(&mut self, _: Vec3, _: Vec3, _: Vec3, _: Vec3, _: &mut Vec<usize>) {}

    fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
        self.0
    }
}

/// A map with a linked pair: blue at the origin facing `+X`, orange far away
/// facing `-X`, both switched on from the start so `Activate` links them.
fn portal_pair_map() -> Vec<bsp::Entity> {
    vec![
        block(&[("classname", "worldspawn")]),
        block(&[
            ("classname", "prop_portal"),
            ("targetname", "blue"),
            ("origin", "0 0 0"),
            ("angles", "0 0 0"),
            ("Activated", "1"),
        ]),
        block(&[
            ("classname", "prop_portal"),
            ("targetname", "orange"),
            ("origin", "1000 0 0"),
            ("angles", "0 180 0"),
            ("PortalTwo", "1"),
            ("Activated", "1"),
        ]),
    ]
}

/// One portal's class state, by name. A free function rather than a closure,
/// for the reason [`branch_list_at`] is one.
fn portal_named<'a>(server: &'a Server, name: &str) -> &'a classes::PropPortal {
    find_named(server, name)
        .behaviour
        .downcast_ref::<classes::PropPortal>()
        .expect("a prop_portal")
}

/// **`PunchAllPenetratingPlayers`.** A portal moved while a player is standing
/// in its partner's plane shoves them out of it, along the **partner's**
/// forward and at 100 units a second — and takes their ground away, or the
/// next `CategorizePosition` would put them straight back.
#[test]
fn moving_a_portal_shoves_a_player_out_of_its_partner() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &portal_pair_map(),
        &[model([-2048.0, -2048.0, -2048.0], [2048.0, 2048.0, 2048.0])],
    );
    assert!(
        portal_named(&server, "blue").is_active_and_linked(),
        "the two portals in the map did not link"
    );

    // Standing in orange's plane: orange is at `(1000, 0, 0)` facing `-X`, so
    // its quad is the plane `x = 1000` and a player hull at that x overlaps it.
    server.spawn_player(player_at(Vec3::new(1000.0, 0.0, -56.0)));

    // Move blue. The shove comes out of orange, which faces `-X`.
    assert!(server.place_portal(false, Vec3::new(0.0, 500.0, 0.0), Vec3::ZERO));
    ticks_touching(&mut server, &mut Embedded(true), 1);

    let state = server.player_state().expect("a player");
    // Orange's yaw of 180 puts a millionth of a unit on the cross axis, which
    // is `sin(180°)` in `f32` and not a sign of anything.
    assert!(
        (state.velocity - Vec3::new(-100.0, 0.0, 0.0)).length() < 1e-3,
        "shoved along orange's forward at PUNCH_SPEED: {}",
        state.velocity
    );
    assert!(!state.on_ground, "VelocityPunch clears the ground first");
}

/// The three guards on the punch, each on its own: the world has to say the
/// player is stuck, the moved portal must not be in the floor, and the pair
/// must already have been a pair.
#[test]
fn the_punch_is_refused_by_each_of_its_three_guards() {
    let punched = |embedded: bool, angles: Vec3, link: bool| {
        let mut server = Server::new();
        let mut map = portal_pair_map();
        if !link {
            // Orange never switches on, so blue has no partner to punch from.
            map[2]
                .pairs
                .iter_mut()
                .find(|(k, _)| k == "Activated")
                .expect("Activated")
                .1 = String::from("0");
        }
        server.level_init(
            "test",
            &map,
            &[model([-2048.0, -2048.0, -2048.0], [2048.0, 2048.0, 2048.0])],
        );
        server.spawn_player(player_at(Vec3::new(1000.0, 0.0, -56.0)));
        assert!(server.place_portal(false, Vec3::new(0.0, 500.0, 0.0), angles));
        ticks_touching(&mut server, &mut Embedded(embedded), 1);
        server.player_state().expect("a player").velocity != Vec3::ZERO
    };

    // `angles` of `-90 0 0` points forward straight up: a floor portal.
    let floor = Vec3::new(-90.0, 0.0, 0.0);
    assert!(punched(true, Vec3::ZERO, true), "the control does punch");
    assert!(
        !punched(false, Vec3::ZERO, true),
        "the world said the player was not stuck and it punched anyway"
    );
    assert!(
        !punched(true, floor, true),
        "a floor portal punched — that is the floor-to-floor exploit"
    );
    assert!(
        !punched(true, Vec3::ZERO, false),
        "a portal with no partner before the move punched"
    );
}

/// **The two animation clocks are not the same clock.** A portal that *moves*
/// re-opens without clearing its own static; one that switches *on* resets
/// both; and either event fills the **partner** with static without re-opening
/// it.
#[test]
fn moving_a_portal_reopens_it_and_fills_its_partner_with_static() {
    let mut server = Server::new();
    server.level_init(
        "test",
        &portal_pair_map(),
        &[model([-2048.0, -2048.0, -2048.0], [2048.0, 2048.0, 2048.0])],
    );

    // Let both settle: a second of server time is past the end of both curves.
    ticks_touching(&mut server, &mut NoTouchQuery, 64);
    let settled = server.time().curtime;

    let (before_opened, before_static) = {
        let orange = portal_named(&server, "orange");
        (orange.opened_at, orange.static_at)
    };
    assert!(server.place_portal(false, Vec3::new(0.0, 500.0, 0.0), Vec3::ZERO));

    {
        let blue = portal_named(&server, "blue");
        assert_eq!(blue.opened_at, settled, "the moved portal re-opened");
        assert_eq!(
            blue.static_at, before_static,
            "OnPortalMoved resets the open amount and not the static"
        );
    }
    let orange = portal_named(&server, "orange");
    assert_eq!(
        orange.opened_at, before_opened,
        "the partner did not move, so it did not re-open"
    );
    assert_eq!(
        orange.static_at, settled,
        "…but it did fill with static: 'add static to the remote'"
    );
}

/// Switching a portal on resets **both** of its clocks, which is the one case
/// `OnActiveStateChanged` differs from `OnPortalMoved` in.
#[test]
fn switching_a_portal_on_resets_both_of_its_clocks() {
    let mut map = portal_pair_map();
    for portal in &mut map[1..] {
        portal
            .pairs
            .iter_mut()
            .find(|(k, _)| k == "Activated")
            .expect("Activated")
            .1 = String::from("0");
    }
    let mut server = Server::new();
    server.level_init(
        "test",
        &map,
        &[model([-2048.0, -2048.0, -2048.0], [2048.0, 2048.0, 2048.0])],
    );
    ticks_touching(&mut server, &mut NoTouchQuery, 64);

    server.queue.add(Event {
        fire_time: server.time().curtime,
        target: Target::Name(String::from("blue")),
        input: String::from("SetActivatedState"),
        value: Variant::Bool(true),
        activator: None,
        caller: None,
        output_id: 0,
    });
    ticks_touching(&mut server, &mut NoTouchQuery, 1);

    // The event is serviced on the tick *after* the one that queued it, so
    // "now" is the clock as it stands after that tick and not before it.
    let now = server.time().curtime;
    let blue = portal_named(&server, "blue");
    assert_eq!((blue.opened_at, blue.static_at), (now, now));
    assert!(blue.linked.is_none(), "orange is still off");
}

// ---------------------------------------------------------------------------
// the pusher — `src/server/push.rs`
// ---------------------------------------------------------------------------

/// A [`TouchQuery`] with a pusher's three clip chains over axis-aligned boxes.
///
/// [`BoxTriggers`] is the same idea for the touch test: the *shape* of the
/// engine's answer with none of its geometry, so that a test about what the
/// pusher decides is not also a test of the BSP sweep. The real one is
/// `crate::engine::push_trace`, and the depot test below runs the pusher
/// against *that* over every shipped map.
///
/// # What it models, and what it does not
///
/// Overlap is **strict** — touching is not overlapping — which is what makes a
/// player resting exactly on a platform not also stuck inside it, and it is
/// the same convention `DIST_EPSILON` buys in the real sweep.
///
/// A blocked sweep stops dead at its start rather than sliding to the contact
/// point: there is no fraction between 0 and 1 here. That is a coarser answer
/// than the engine's and it is the conservative one — a push that the real
/// trace would have let through part way is refused outright — so a test that
/// passes against this is not relying on a partial shove.
///
/// Angles are handled by bounding the rotated box, which is exact at right
/// angles and conservative in between.
struct PushBoxes {
    /// `("*N" index, model-space mins, model-space maxs)` — the brush models
    /// the pushers name.
    models: Vec<(usize, Vec3, Vec3)>,
    /// World-space solid boxes that are not entities: the walls a push can
    /// squash something against.
    world: Vec<(Vec3, Vec3)>,
}

impl PushBoxes {
    fn new(models: &[(usize, Vec3, Vec3)], world: &[(Vec3, Vec3)]) -> PushBoxes {
        PushBoxes {
            models: models.to_vec(),
            world: world.to_vec(),
        }
    }

    /// Does a hull at `origin` overlap this pusher's brush model?
    ///
    /// [`swept_box_touches_obb`](crate::server::obb::swept_box_touches_obb) —
    /// the real `IntersectRayWithOBB`, which the port already has for triggers
    /// — rather than a bounded AABB, because a `func_door_rotating`'s bounding
    /// box at 45° is nearly twice the slab and a test built on it would report
    /// contact with thin air.
    fn touches(
        &self,
        pusher: &crate::server::Pusher,
        origin: Vec3,
        mins: Vec3,
        maxs: Vec3,
    ) -> bool {
        let Some(&(_, obb_mins, obb_maxs)) =
            self.models.iter().find(|(i, _, _)| *i == pusher.model)
        else {
            return false;
        };
        crate::server::obb::swept_box_touches_obb(
            origin,
            origin,
            mins,
            maxs,
            pusher.origin,
            pusher.angles,
            obb_mins,
            obb_maxs,
        )
    }
}

/// Strictly overlapping, on all three axes.
fn boxes_overlap(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> bool {
    (0..3).all(|i| a.0[i] < b.1[i] && a.1[i] > b.0[i])
}

impl TouchQuery for PushBoxes {
    fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
        false
    }

    fn brush_models_touching(&mut self, _: Vec3, _: Vec3, _: Vec3, _: Vec3, _: &mut Vec<usize>) {}

    fn push_trace(
        &mut self,
        clip: crate::server::PushClip,
        start: Vec3,
        end: Vec3,
        mins: Vec3,
        maxs: Vec3,
        pushers: &[crate::server::Pusher],
    ) -> crate::server::PushHit {
        use crate::server::PushClip;

        let world = clip != PushClip::PushersOnly;
        let entities = clip != PushClip::WithoutPushers;

        let hits = |origin: Vec3| {
            let hull = (origin + mins, origin + maxs);
            (world && self.world.iter().any(|&b| boxes_overlap(hull, b)))
                || (entities
                    && pushers
                        .iter()
                        .any(|pusher| self.touches(pusher, origin, mins, maxs)))
        };

        let start_solid = hits(start);
        match start_solid || hits(end) {
            true => crate::server::PushHit {
                fraction: 0.0,
                end: start,
                start_solid,
            },
            false => crate::server::PushHit {
                fraction: 1.0,
                end,
                start_solid,
            },
        }
    }
}

/// The player, standing on the floor at `origin` and nothing else —
/// [`player_at`] with the hull spelled out so the arithmetic in these tests is
/// readable: `x ± 16`, `z + 0..72`.
fn pushed_player(origin: Vec3) -> PlayerState {
    player_at(origin)
}

/// A `func_door` sliding along `+X`, with `extra` keys layered on.
///
/// Model `*1` is [`door_models`]' 66×10×66 slab, so `CBaseDoor::Spawn`'s
/// `vecOBB -= Vector(2,2,2)` leaves a travel of exactly **64** and a `speed`
/// of 64 makes it **one unit a tick**. Every position assertion below is
/// counted in those units.
fn sliding_door_map(extra: &[(&str, &str)]) -> Vec<bsp::Entity> {
    let blocked = conn("blocked", "Add", "1", "0", "-1");
    let unblocked = conn("unblocked", "Add", "1", "0", "-1");
    let mut pairs: Vec<(&str, &str)> = vec![
        ("classname", "func_door"),
        ("targetname", "door"),
        ("model", "*1"),
        ("origin", "0 0 0"),
        // `AngleVectors( 0 0 0 )` is `+X`.
        ("movedir", "0 0 0"),
        ("lip", "0"),
        ("speed", "64"),
        ("OnBlockedOpening", &blocked),
        ("OnUnblockedOpening", &unblocked),
    ];
    pairs.extend_from_slice(extra);
    vec![
        block(&pairs),
        block(&[("classname", "math_counter"), ("targetname", "blocked")]),
        block(&[("classname", "math_counter"), ("targetname", "unblocked")]),
    ]
}

/// The door's box, in world space, for the assertions below.
fn door_x(server: &Server) -> f32 {
    placement(server, "door").0.x
}

fn player_x(server: &Server) -> f32 {
    server.player_state().expect("a player").origin.x
}

/// **The headline: a door told to open shoves the player out of its way.**
///
/// The door's leading face starts at x = 33 and the player's trailing face at
/// x = 44, so nothing happens for the first eleven ticks; from then on the
/// player travels exactly as far as the door does, and ends up 64 − 11 units
/// downrange of where they began.
#[test]
fn an_opening_door_pushes_the_player_along_in_front_of_it() {
    let map = sliding_door_map(&[("wait", "-1")]);
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    server.spawn_player(pushed_player(Vec3::new(60.0, 0.0, -33.0)));

    let mut query = PushBoxes::new(
        &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
        &[],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    // Ten ticks: the door has moved ten units and has not reached the player.
    ticks_touching(&mut server, &mut query, 10);
    assert!((door_x(&server) - 10.0).abs() < 0.01, "{}", door_x(&server));
    assert_eq!(player_x(&server), 60.0, "not touched yet");

    // All the way open. The player has been carried the rest of the travel.
    ticks_touching(&mut server, &mut query, 60);
    assert!((door_x(&server) - 64.0).abs() < 0.01, "{}", door_x(&server));
    assert!(
        (player_x(&server) - 113.0).abs() < 1.5,
        "the player should be in front of the door, not at {}",
        player_x(&server)
    );
    assert_eq!(counter_value(&server, "blocked"), 0.0, "never blocked");
}

/// **The other half: a player with their back to a wall stops the door.**
///
/// The same door, with a wall two units behind the player. The push is refused,
/// the player is put back, the door is put back, and the move is retried from
/// the same place next tick — for ever, because this door's `wait` is negative
/// and `CBaseDoor::Blocked` refuses to reverse one of those.
#[test]
fn a_player_against_a_wall_blocks_the_door_and_holds_its_clock() {
    let map = sliding_door_map(&[("wait", "-1")]);
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    server.spawn_player(pushed_player(Vec3::new(60.0, 0.0, -33.0)));

    let mut query = PushBoxes::new(
        &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
        // A wall starting two units past the player's back.
        &[(Vec3::new(78.0, -64.0, -64.0), Vec3::new(300.0, 64.0, 64.0))],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    // Two whole seconds — twice the travel time — and it is still short of its
    // destination, jammed a unit or two past first contact.
    ticks_touching(&mut server, &mut query, 128);
    let stalled = door_x(&server);
    assert!(
        (11.0..16.0).contains(&stalled),
        "the door should be stuck just past first contact, not at {stalled}"
    );
    assert_eq!(
        counter_value(&server, "blocked"),
        1.0,
        "one edge, not one a tick"
    );

    // The blocker is remembered, and it is the player.
    let blocker = find_named(&server, "door").blocker;
    assert_eq!(blocker, server.player(), "the player is the blocker");

    // Let them out of the way: the door resumes from where it stopped rather
    // than catching up, because its local time never advanced.
    server.set_player_state(pushed_player(Vec3::new(400.0, 0.0, -33.0)));
    ticks_touching(&mut server, &mut query, 1);
    assert_eq!(
        counter_value(&server, "unblocked"),
        1.0,
        "OnUnblockedOpening"
    );
    assert_eq!(find_named(&server, "door").blocker, None);

    ticks_touching(&mut server, &mut query, 64);
    assert!((door_x(&server) - 64.0).abs() < 0.01, "{}", door_x(&server));
}

/// A blocked door whose `wait` is not negative turns round instead —
/// `CBaseDoor::Blocked`'s `if (m_flWait >= 0)`. 118 of the game's 621 doors
/// take this branch and 503 take the one above.
#[test]
fn a_blocked_door_with_a_wait_reverses() {
    let map = sliding_door_map(&[("wait", "3")]);
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    server.spawn_player(pushed_player(Vec3::new(60.0, 0.0, -33.0)));

    let mut query = PushBoxes::new(
        &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
        &[(Vec3::new(78.0, -64.0, -64.0), Vec3::new(300.0, 64.0, 64.0))],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    ticks_touching(&mut server, &mut query, 20);
    assert_eq!(counter_value(&server, "blocked"), 1.0);

    // It went back rather than staying jammed.
    ticks_touching(&mut server, &mut query, 44);
    assert!(
        door_x(&server) < 1.0,
        "a reversed door goes home, not to {}",
        door_x(&server)
    );
}

/// **A platform carries what is standing on it.**
///
/// `IsStandingOnPusher` is the branch that does it, and it is not an
/// optimisation: something resting *on* a mover is beside it, not inside it,
/// so the interpenetration test says no and only the ground test says yes.
#[test]
fn a_platform_carries_a_player_standing_on_top_of_it() {
    let map = sliding_door_map(&[("wait", "-1")]);
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    // Feet exactly on the slab's top face, which is z = 33.
    server.spawn_player(pushed_player(Vec3::new(0.0, 0.0, 33.0)));

    let mut query = PushBoxes::new(
        &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
        &[],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);

    ticks_touching(&mut server, &mut query, 64);
    assert!((door_x(&server) - 64.0).abs() < 0.01, "{}", door_x(&server));
    assert!(
        (player_x(&server) - 64.0).abs() < 1.5,
        "the rider should have gone with it, not stayed at {}",
        player_x(&server)
    );
}

/// Neither of the two doors the player walks through pushes them —
/// `SF_DOOR_PASSABLE` makes the door non-solid and
/// `SF_DOOR_NONSOLID_TO_PLAYER` puts it in `COLLISION_GROUP_PASSABLE_DOOR`.
/// **257 of the game's 621 doors set one or the other.**
#[test]
fn a_door_the_player_can_walk_through_neither_pushes_nor_is_blocked() {
    for (flag, what) in [(8, "SF_DOOR_PASSABLE"), (4, "SF_DOOR_NONSOLID_TO_PLAYER")] {
        let spawnflags = flag.to_string();
        let map = sliding_door_map(&[("wait", "-1"), ("spawnflags", &spawnflags)]);
        let mut server = Server::new();
        server.level_init("test", &map, &door_models());
        server.spawn_player(pushed_player(Vec3::new(60.0, 0.0, -33.0)));

        let mut query = PushBoxes::new(
            &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
            &[(Vec3::new(78.0, -64.0, -64.0), Vec3::new(300.0, 64.0, 64.0))],
        );

        let door = find_named(&server, "door").id();
        server.accept_input(door, "Open", Variant::Void, None, None, 0);
        ticks_touching(&mut server, &mut query, 70);

        assert!(
            (door_x(&server) - 64.0).abs() < 0.01,
            "{what}: the door should have opened through the player, not stopped at {}",
            door_x(&server)
        );
        assert_eq!(player_x(&server), 60.0, "{what}: the player was not moved");
        assert_eq!(
            counter_value(&server, "blocked"),
            0.0,
            "{what}: never blocked"
        );
    }
}

/// A rotating door sweeps its blocker **sideways**, along the arc rather than
/// along any axis — `ComputeRotationalPushDirection`, which is the only way to
/// get a translation out of a rotation for something that does not itself
/// turn.
///
/// # Which side of the hinge the player is on decides the outcome
///
/// The push is measured from **one corner** of the blocker's world box, and
/// with a zero previous push that is the low corner on all three axes — see
/// [`push`](crate::server::push)'s finding 2. A leaf swinging counterclockwise
/// from `+X` sweeps the first quadrant with one end and the third with the
/// other, and the low corner's radius is the *smallest* of the four in the
/// first quadrant and the *largest* in the third. So the same door under-pushes
/// on one side and over-pushes on the other; this test takes the third
/// quadrant, and [`a_rotating_door_under_pushes_on_the_other_side_and_jams`]
/// pins the other.
#[test]
fn a_rotating_door_sweeps_the_player_around_its_hinge() {
    let map = vec![
        block(&[
            ("classname", "func_door_rotating"),
            ("targetname", "door"),
            ("model", "*1"),
            ("origin", "0 0 0"),
            ("distance", "90"),
            ("speed", "90"),
            ("wait", "-1"),
        ]),
        block(&[("classname", "math_counter"), ("targetname", "blocked")]),
    ];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    // Out in the quadrant the leaf swings through, and clear of it at rest:
    // the slab lies along X when closed and reaches this corner at about 45
    // degrees.
    server.spawn_player(pushed_player(Vec3::new(-30.0, -30.0, -33.0)));

    let mut query = PushBoxes::new(
        &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
        &[],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    ticks_touching(&mut server, &mut query, 64);

    let moved = server.player_state().expect("a player").origin;
    // Swept **along the arc**: counterclockwise about the hinge is −X and +Y
    // together, which is what tells a rotational push apart from a linear one.
    assert!(
        moved.x > -29.0 && moved.y < -31.0,
        "the leaf should have swept the player along its arc, not left them at {moved}"
    );
}

/// A push moves the pusher's **children** too, and something standing in front
/// of a child is pushed by the hierarchy as a whole —
/// `SetupAllInHierarchy` is what puts them in the pusher list.
#[test]
fn a_child_of_a_moving_door_pushes_as_the_door_does() {
    let map = vec![
        block(&[
            ("classname", "func_door"),
            ("targetname", "door"),
            ("model", "*1"),
            ("origin", "0 0 0"),
            ("movedir", "0 0 0"),
            ("lip", "0"),
            ("speed", "64"),
            ("wait", "-1"),
        ]),
        // A second slab bolted to the first, 66 units further along +X, so it
        // is what reaches the player first.
        block(&[
            ("classname", "func_brush"),
            ("targetname", "rider"),
            ("parentname", "door"),
            ("model", "*2"),
            ("origin", "66 0 0"),
        ]),
    ];
    let models = vec![
        model([-512.0, -512.0, -512.0], [512.0, 512.0, 512.0]),
        model([-33.0, -5.0, -33.0], [33.0, 5.0, 33.0]),
        model([-33.0, -5.0, -33.0], [33.0, 5.0, 33.0]),
    ];

    let mut server = Server::new();
    server.level_init("test", &map, &models);
    // The child's leading face is at x = 99; the player's back is at 110.
    server.spawn_player(pushed_player(Vec3::new(126.0, 0.0, -33.0)));

    let mut query = PushBoxes::new(
        &[
            (1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0)),
            (2, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0)),
        ],
        &[],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    ticks_touching(&mut server, &mut query, 70);

    assert!((door_x(&server) - 64.0).abs() < 0.01, "{}", door_x(&server));
    assert!(
        (placement(&server, "rider").0.x - 130.0).abs() < 0.01,
        "the child rode the parent"
    );
    assert!(
        (player_x(&server) - 179.0).abs() < 1.5,
        "the child pushed the player, not {}",
        player_x(&server)
    );
}

/// The other side of the hinge, and the reason
/// [`a_rotating_door_sweeps_the_player_around_its_hinge`] has to say which
/// quadrant it stands in.
///
/// In the first quadrant the low corner of the player's box is the one
/// **nearest** the hinge, so the arc it computes is shorter than the one the
/// contact point actually travels — the player is pushed, and is still inside
/// the leaf when they get there. `IsPushedPositionValid` then refuses, the
/// whole tick is rolled back, and the door jams a few degrees into its swing.
///
/// This is Valve's `Vector vecAbsPush;` — uninitialised on the first pass —
/// reproduced with a zero in place of the stack garbage. It is **not** a
/// choice this port made, and it is the reason the reference's own comment on
/// the branch reads *"BUGBUG: This will break, but not as badly as the
/// previous solution!!!"*.
#[test]
fn a_rotating_door_under_pushes_on_the_other_side_and_jams() {
    let map = vec![block(&[
        ("classname", "func_door_rotating"),
        ("targetname", "door"),
        ("model", "*1"),
        ("origin", "0 0 0"),
        ("distance", "90"),
        ("speed", "90"),
        ("wait", "-1"),
    ])];
    let mut server = Server::new();
    server.level_init("test", &map, &door_models());
    server.spawn_player(pushed_player(Vec3::new(30.0, 30.0, -33.0)));

    let mut query = PushBoxes::new(
        &[(1, Vec3::new(-33.0, -5.0, -33.0), Vec3::new(33.0, 5.0, 33.0))],
        &[],
    );

    let door = find_named(&server, "door").id();
    server.accept_input(door, "Open", Variant::Void, None, None, 0);
    ticks_touching(&mut server, &mut query, 128);

    let yaw = placement(&server, "door").1.y;
    assert!(
        (5.0..45.0).contains(&yaw),
        "the leaf should have jammed part way, not reached {yaw}"
    );
    assert_eq!(
        find_named(&server, "door").blocker,
        server.player(),
        "and it knows who did it"
    );
    // The roll-back put them back: a blocked push leaves nothing behind.
    close(
        server.player_state().expect("a player").origin,
        Vec3::new(30.0, 30.0, -33.0),
    );
}

/// **Every linear mover in the shipped game, with a player standing in front
/// of it.**
///
/// The stage's headline measurement, and the one that could not be faked: for
/// each `func_door` and `func_movelinear` the port has a class for, it works
/// out which way the mover travels by opening it once and watching, then puts
/// a real player hull two units clear of the face that is coming and opens it
/// again — through
/// [`crate::engine::push_trace`], the same three sweeps the running game uses.
/// Each mover then lands in exactly one of four buckets:
///
/// - **pushed** — the player was carried out of the way, which is the feature;
/// - **blocked** — the player could not be moved and the mover is jammed
///   against them, with `m_pBlocker` naming the player;
/// - **missed** — the mover travelled and never reached the probe, which is a
///   mover whose swept volume does not pass through the point in front of its
///   own bounding box (an L-shaped brush model, or one that slides *along* its
///   long axis);
/// - **no room** — the probe point is inside the world or another brush model,
///   so there is nowhere for a player to stand and be pushed from.
///
/// Rotating doors are **not** here: the probe is "in front of the face that is
/// coming", and a leaf that turns in place has no such face until you know its
/// hinge. They are covered by
/// [`a_rotating_door_sweeps_the_player_around_its_hinge`] and its jamming
/// twin, which are where the quadrant asymmetry of Valve's corner hack is
/// pinned.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_mover -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_mover_pushes_the_player_standing_in_front_of_it() {
    use crate::engine::trace::CollisionBsp;
    use crate::engine::world::{bsp::Bsp, find_brush_models, PlacedBrushModel};

    let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
        panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
    };
    let dir = std::path::PathBuf::from(dir);
    let base = dir.parent().unwrap_or(&dir).to_path_buf();
    let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
        .expect("the game mounts");
    let mut names: Vec<String> = vfs
        .list("maps")
        .expect("maps/")
        .into_iter()
        .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
        .map(|e| e.name.trim_end_matches(".bsp").to_owned())
        .collect();
    names.sort();

    /// Long enough for a shipped mover to finish: the slowest travel in the
    /// game is under two seconds, and a jammed one is jammed by the first
    /// contact.
    const SECONDS: f32 = 2.0;
    /// How far a mover has to travel before it is worth probing. Below this a
    /// mover cannot clear a 32-unit-wide player hull anyway; it is also what
    /// takes out the 53 `SF_BUTTON_DONTMOVE`-shaped zero travels.
    const MIN_TRAVEL: f32 = 8.0;

    let mut candidates = 0usize;
    let mut pushed = 0usize;
    let mut blocked = 0usize;
    let mut missed = 0usize;
    let mut no_room = 0usize;
    // Doors the player walks through: `SF_DOOR_PASSABLE` or
    // `SF_DOOR_NONSOLID_TO_PLAYER`, counted rather than probed.
    let mut passable = 0usize;
    let mut furthest = 0.0f32;

    for name in &names {
        let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
        let collision = CollisionBsp::build(&bsp);
        let entities = bsp.entities();
        let placed = find_brush_models(&entities, &collision);

        // Pass one: open everything and watch where it goes, which is the only
        // way to learn a mover's travel direction without reading its class's
        // private state.
        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);

        let mut movers: Vec<(EntityId, Vec3)> = Vec::new();
        for (id, entity) in server.entities.iter() {
            if !matches!(entity.classname(), "func_door" | "func_movelinear") {
                continue;
            }
            if entity.core.brush_model_index().is_none() {
                continue;
            }
            if !entity.core.is_solid() || !entity.core.collides_with_player() {
                passable += 1;
                continue;
            }
            movers.push((id, entity.core.origin));
        }

        for &(id, _) in &movers {
            server.accept_input(id, "Open", Variant::Void, None, None, 0);
        }
        let interval = server.time().interval;
        let ticks = (SECONDS / interval).round() as u32;
        for _ in 0..ticks {
            server.frame(interval, &mut NoTouchQuery);
        }

        let travels: Vec<(EntityId, Vec3, Vec3)> = movers
            .iter()
            .filter_map(|&(id, spawn)| {
                let now = server.entities.get(id)?.core.origin;
                Some((id, spawn, now - spawn))
            })
            .filter(|(_, _, travel)| travel.length() > MIN_TRAVEL)
            .collect();
        if travels.is_empty() {
            continue;
        }

        // Pass two: a fresh level, and one mover at a time.
        let mut server = Server::new();
        server.level_init(name, &entities, &bsp.models);
        server.spawn_player(player_at(Vec3::ZERO));

        // `sync_brush_models`' body, which the running game does once a
        // frame and this does whenever the movers have moved: **`find_brush_models`
        // leaves every model solid and unowned**, so a chain built straight
        // from it holds every trigger in the map as a wall and there is
        // nowhere in a test chamber a player can stand.
        let synced = |server: &Server| -> Vec<PlacedBrushModel> {
            placed
                .iter()
                .filter_map(|p| {
                    let entity = server.brush_entity(p.index)?;
                    let mut p = p.clone();
                    p.owned = true;
                    p.solid = entity.is_solid() && entity.collides_with_player();
                    p.model.set_placement(entity.origin, entity.angles);
                    Some(p)
                })
                .collect()
        };

        for (id, spawn, travel) in travels {
            candidates += 1;
            let Some(entity) = server.entities.get(id) else {
                continue;
            };
            let bounds = entity.core.model_bounds;
            let direction = travel.normalize();

            // **Stand in the doorway and let the door close on you.**
            //
            // That is the one probe the shipped content admits, and working
            // out why is most of what this test is worth. A Portal 2 door
            // slides along its own long axis into a pocket in the wall, so "in
            // front of the leading face" is inside that wall and "where it is
            // going" is inside the pocket. The only place on a mover's path a
            // player can stand is the place the mover *itself* occupies when
            // shut — so it is opened first, and then told to close over the
            // space it has just vacated.
            let extent = (bounds.maxs - bounds.mins) * 0.5;
            let centre = spawn + (bounds.mins + bounds.maxs) * 0.5;

            // A small lattice rather than the centre alone: a mover's box
            // reaches into the floor and into the frame either side of it, so
            // its middle is very often inside something. Same reason
            // `probe_inside` lattices a trigger; the first point a standing
            // hull fits in wins.
            let lateral: Vec<Vec3> = [Vec3::X, Vec3::Y, Vec3::Z]
                .into_iter()
                .filter(|axis| axis.dot(direction).abs() < 0.9 && axis.z == 0.0)
                .collect();
            let mut probes: Vec<Vec3> = Vec::new();
            for &up in &[1.0f32, 8.0, 24.0] {
                for across in [0.0f32, 0.4, -0.4] {
                    let mut probe = centre;
                    for &axis in &lateral {
                        probe += axis * (across * axis.dot(extent).abs());
                    }
                    probe.z = spawn.z + bounds.mins.z + up;
                    probes.push(probe);
                }
            }

            // Open it and let it finish, with the player parked far away, so
            // that the doorway is empty before anybody stands in it.
            server.set_player_state(player_at(Vec3::splat(1.0e6)));
            server.accept_input(id, "Open", Variant::Void, None, None, 0);
            {
                let owned = synced(&server);
                let mut query = Placed {
                    collision: &collision,
                    models: &owned,
                    chain: Vec::new(),
                };
                for _ in 0..ticks {
                    server.frame(interval, &mut query);
                }
            }

            // …and the clip chain has to be rebuilt now that it has moved, or
            // the room test below finds the door still standing where it was.
            let owned = synced(&server);
            let mut query = Placed {
                collision: &collision,
                models: &owned,
                chain: Vec::new(),
            };

            let hull = player_at(Vec3::ZERO);
            let Some(probe) = probes.into_iter().find(|&probe| {
                !query
                    .push_trace(
                        crate::server::PushClip::Everything,
                        probe,
                        probe,
                        hull.mins,
                        hull.maxs,
                        &[],
                    )
                    .start_solid
            }) else {
                // Nowhere to stand: the world, or another brush model, is there.
                no_room += 1;
                continue;
            };

            server.set_player_state(player_at(probe));
            server.accept_input(id, "Close", Variant::Void, None, None, 0);
            for _ in 0..ticks {
                server.frame(interval, &mut query);
            }

            let moved = server
                .player_state()
                .map_or(0.0, |state| (state.origin - probe).length());
            let jammed = server
                .entities
                .get(id)
                .is_some_and(|e| e.core.blocker == server.player() && e.core.blocker.is_some());
            furthest = furthest.max(moved);
            match (moved > 1.0, jammed) {
                (true, _) => pushed += 1,
                (false, true) => blocked += 1,
                (false, false) => missed += 1,
            }
        }
    }

    println!("\n{} maps", names.len());
    println!(
        "  {candidates} linear movers probed: {pushed} pushed the player, \
         {blocked} were blocked by them,\n  \
         {missed} never reached the probe, {no_room} had nowhere to stand;\n  \
         {passable} more are doors the player walks through and cannot be pushed by."
    );
    println!("  furthest a mover shoved the player: {furthest:.1} units");

    // Exact, the way the other depot tests are exact: the seed is fixed and
    // the maps do not change, so a number that moves is a behaviour that
    // moved. **67 of the 263 engaged the pusher** — 54 shoved the player out
    // of the doorway and 13 were stopped dead by them.
    assert_eq!(
        (pushed, blocked, missed, no_room, passable),
        (54, 13, 52, 144, 163),
        "the pusher's census over the shipped maps has changed"
    );
    assert!(
        (234.0..235.0).contains(&furthest),
        "the furthest shove was {furthest:.1} units"
    );
}

/// **Every `SetParentAttachment*` the shipped game fires, followed through to
/// where it puts the entity.**
///
/// `LookupAttachment`'s depot test, and the one that says what the feature is
/// worth. For each of the 106 maps it spawns the entities, loads every `.mdl`
/// their props name — through `StudioModel`, which needs a `Vfs` and no GPU —
/// fills in the sequence table *and* the attachment table exactly as
/// `Engine::load_level` does, and runs two seconds.
///
/// It uses `engine::world::entities::AttachmentModels` rather than a stand-in,
/// for the reason the pusher's depot test uses the engine's real
/// `push_trace`: a seam measured against a copy of itself is not measured.
///
/// ```text
/// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_attachment -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
fn every_shipped_attachment_connection_puts_its_entity_on_a_bone() {
    use crate::engine::world::entities::AttachmentModels;
    use crate::filesystem::Vfs;
    use crate::studio::StudioModel;

    const RUN_SECONDS: f32 = 2.0;

    /// The engine's table, as the server asks it — `engine/mod.rs`'s
    /// `WorldAttachments`, which is private to that module.
    struct Table(std::sync::Arc<AttachmentModels>);

    impl crate::server::attachment::Attachments for Table {
        fn lookup(&self, model: &str, name: &str) -> Option<usize> {
            self.0.lookup(model, name)
        }

        fn attachment_to_model(
            &self,
            posed: &crate::server::attachment::Posed<'_>,
            attachment: usize,
        ) -> Option<glam::Affine3A> {
            self.0
                .attachment_to_model(
                    posed.model,
                    attachment,
                    posed.sequence,
                    posed.cycle,
                    posed.anim_time,
                    posed.playback_rate,
                    posed.now,
                )
                .map(glam::Affine3A::from_mat4)
        }
    }

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

    // Every `.mdl` read so far. `None` for one that would not load.
    let mut studio: BTreeMap<String, Option<StudioModel>> = BTreeMap::new();

    // Connections the maps declare, counted from the lump rather than from a
    // handler, so the denominator is the content and not the port.
    let mut declared = 0usize;
    // Entities that ended the run riding an attachment point.
    let mut on_a_point = 0usize;
    // …and the three ways Valve's two guards refuse one.
    let (mut no_parent, mut parent_has_no_model, mut no_such_point) = (0usize, 0usize, 0usize);
    // Which classes are parented to a bone, by connection — the shape of the
    // content rather than a total, and the reason `func_brush` dominates is
    // that a Hammer instance parents its clip set to an arm.
    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
    // Entities whose origin moved during the run while their **parent's** did
    // not — the animation carrying them, which plain parenting cannot do.
    let mut carried_by_animation = 0usize;
    let (mut furthest_offset, mut furthest_offset_at) = (0.0f32, String::new());
    let (mut furthest_carry, mut furthest_carry_at) = (0.0f32, String::new());
    let mut maps_with_one = 0usize;

    let mut server = Server::new();
    for name in &names {
        let bsp = crate::engine::world::bsp::Bsp::load(&vfs, name)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let blocks = bsp.entities();

        // The connections the map declares, before anything runs. A value is
        // `target,input,parameter,delay,times` and the input is field 1.
        let here_declared = blocks
            .iter()
            .flat_map(|block| block.pairs.iter())
            .filter(|(_, value)| {
                value
                    .split(&[',', '\x1b'][..])
                    .nth(1)
                    .map(|input| {
                        input.eq_ignore_ascii_case("SetParentAttachment")
                            || input.eq_ignore_ascii_case("SetParentAttachmentMaintainOffset")
                    })
                    .unwrap_or(false)
            })
            .count();
        declared += here_declared;
        if here_declared > 0 {
            maps_with_one += 1;
        }

        server.level_init(name, &blocks, &bsp.models);

        // The engine's job, done here: read every model the entities name and
        // hand both answers back.
        let mut table = sequences::SequenceTable::new();
        let mut attachments = AttachmentModels::default();
        let models: Vec<String> = server
            .entities
            .iter()
            .filter_map(|(_, entity)| entity.core.model.clone())
            .filter(|model| !model.starts_with('*') && !model.is_empty())
            .collect();
        for model in models {
            let entry = studio
                .entry(model.to_ascii_lowercase())
                .or_insert_with(|| StudioModel::load(&vfs, &model).ok());
            let Some(loaded) = entry else { continue };
            table.insert_model(
                &model,
                loaded
                    .sequences
                    .iter()
                    .enumerate()
                    .map(|(i, sequence)| {
                        (
                            sequence.label.clone(),
                            sequences::SequenceInfo {
                                duration: loaded.animation(i).map_or(0.0, |a| a.duration()),
                                loops: sequence.flags & crate::studio::anim::STUDIO_LOOPING != 0,
                                fade_out_time: sequence.fade_out_time,
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            attachments.insert_model(&model, loaded);
        }
        server.set_sequences(table);
        server.set_attachments(Box::new(Table(std::sync::Arc::new(attachments))));

        // One tick to let the `logic_auto` bootstrap fire its parenting, then
        // a snapshot, then the rest of the run — so "moved" means "moved after
        // it was attached" rather than "was placed".
        run(&mut server, 0.1);
        let placed: BTreeMap<u64, (Vec3, Vec3)> = server
            .entities
            .iter()
            .filter(|(_, entity)| entity.core.parent_attachment().is_some())
            .filter_map(|(id, entity)| {
                let parent = server.entities.get(entity.core.parent()?)?;
                Some((id.to_int(), (entity.core.origin, parent.core.origin)))
            })
            .collect();
        run(&mut server, RUN_SECONDS - 0.1);

        for (id, entity) in server.entities.iter() {
            match entity.core.parent_attachment() {
                Some(_) => {
                    on_a_point += 1;
                    if let Some(parent) = entity.core.parent().and_then(|p| server.entities.get(p)) {
                        let where_ = || {
                            format!(
                                "{name}: {} on {}",
                                entity.debug_name(),
                                parent.core.model.clone().unwrap_or_default()
                            )
                        };
                        let offset = (entity.core.origin - parent.core.origin).length();
                        if offset > furthest_offset {
                            furthest_offset = offset;
                            furthest_offset_at = where_();
                        }
                        if let Some(&(was, parent_was)) = placed.get(&id.to_int()) {
                            let moved = (entity.core.origin - was).length();
                            // The parent standing still is what makes this the
                            // *animation's* doing: plain parenting could not
                            // have moved the child at all.
                            if moved > 0.01 && (parent.core.origin - parent_was).length() < 0.01 {
                                carried_by_animation += 1;
                                if moved > furthest_carry {
                                    furthest_carry = moved;
                                    furthest_carry_at = where_();
                                }
                            }
                        }
                    }
                }
                // The refusals, told apart the way `SetParentAttachment`'s two
                // guards tell them apart. Only entities a map actually aimed
                // one at can be counted, and the port does not record which
                // those were — so this is the *shape* of the population rather
                // than a per-connection tally.
                None => {}
            }
        }

        // The three refusal counts, from the lump: for each connection, what
        // the target's `parentname` resolves to.
        for block in &blocks {
            for (_, value) in &block.pairs {
                let mut fields = value.split(&[',', '\x1b'][..]);
                let (Some(target), Some(input)) = (fields.next(), fields.next()) else {
                    continue;
                };
                if !input.eq_ignore_ascii_case("SetParentAttachment")
                    && !input.eq_ignore_ascii_case("SetParentAttachmentMaintainOffset")
                {
                    continue;
                }
                let Some(id) = name::find_by_name(&server.entities, target).next() else {
                    continue;
                };
                let Some(entity) = server.entities.get(id) else {
                    continue;
                };
                *by_class
                    .entry(entity.classname().to_owned())
                    .or_default() += 1;
                if entity.core.parent_attachment().is_some() {
                    continue;
                }
                match entity.core.parent() {
                    None => no_parent += 1,
                    Some(parent) => {
                        let has_model = server
                            .entities
                            .get(parent)
                            .and_then(|p| p.core.model.clone())
                            .map(|m| !m.starts_with('*') && !m.is_empty())
                            .unwrap_or(false);
                        match has_model {
                            true => no_such_point += 1,
                            false => parent_has_no_model += 1,
                        }
                    }
                }
            }
        }
    }

    println!(
        "attachment parenting over {} maps ({maps_with_one} with any):\n  \
         {declared} SetParentAttachment* connections declared;\n  \
         {on_a_point} entities ended up riding a bone, the furthest \
         {furthest_offset:.1} units from its parent's origin ({furthest_offset_at});\n  \
         {carried_by_animation} of those were carried by the animation alone \
         (furthest {furthest_carry:.1} units, {furthest_carry_at});\n  \
         refused: {no_parent} with no parent, {parent_has_no_model} whose parent \
         is not a studio model, {no_such_point} with no point by that name.",
        names.len()
    );
    let mut ranked: Vec<(&String, &usize)> = by_class.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("  by the class being parented:");
    for (classname, count) in ranked.iter().take(10) {
        println!("    {count:>7}  {classname}");
    }

    // Exact, the way the other depot tests are exact: the seed is fixed and
    // the maps do not change, so a number that moves is a behaviour that
    // moved.
    assert_eq!(
        (
            declared,
            on_a_point,
            carried_by_animation,
            no_parent,
            parent_has_no_model,
            no_such_point
        ),
        (1362, 1040, 6, 6, 0, 174),
        "the attachment census over the shipped maps has changed"
    );
}
