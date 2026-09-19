//! Rigid-body physics — `legacy/vphysics/` (22,607 lines) and the whole of the
//! `legacy/ivp` submodule, on [Rapier].
//!
//! `portdocs/VPHYSICS.md` is the port plan and the evidence behind every
//! decision here; `rustdocs/VPHYSICS.md` is how to use what landed. The short
//! form, because it is the thing most likely to surprise:
//!
//! - [`collide`] reads Valve's format — `.phy` files and a `.bsp`'s
//!   `LUMP_PHYSCOLLIDE` — into convex hulls. This is the only part of IVP that
//!   is *ported*; every engine needs those hulls and none can read them.
//! - [`surfaceprops`] is the friction and elasticity database.
//! - [`env`](mod@env) is the simulation, and **it runs in Source units** rather than in
//!   Valve's metres. Rapier's `length_unit` is the reason it can.
//!
//! [Rapier]: https://rapier.rs

pub mod collide;
pub mod env;
pub mod surfaceprops;

use collide::{SolidParams, VCollide};
use env::{Hulls, Mass};

/// A collision model, read once and placed any number of times.
///
/// `vcollide_t` reduced to the solid a body actually uses — `CPhysCollide *`
/// plus the `solid { }` block beside it. `PhysModelCreate`
/// (`physics_shared.cpp:302`) takes exactly these three things and hands them
/// to `CreatePolyObject`.
#[derive(Debug, Clone)]
pub struct Model {
    /// The convex pieces, shared: cloning this clones reference counts.
    pub hulls: Hulls,
    /// The first `solid { }` block, which is the one
    /// `PhysModelParseSolidByIndex` takes.
    pub params: SolidParams,
    /// Mass, mass centre and inertia, already through
    /// `portdocs/VPHYSICS.md` §3.3's arithmetic.
    pub mass: Mass,
    /// Whether the collision model carries **more than one solid** — one per
    /// bone, which is what `studiomdl` writes for a `$collisionjoints` model.
    ///
    /// Valve's route for one is `CreateBoneFollowers`, a separate solid entity
    /// per joint that tracks the animation; the prop itself goes
    /// `FSOLID_NOT_SOLID`. Nothing here does that, so a consumer that cannot
    /// animate its collision should refuse a jointed model rather than freeze
    /// solid 0 of it in the bind pose. **51 of Portal 2's 1,056 collision
    /// models are jointed**, up to 23 solids.
    pub jointed: bool,
}

impl Model {
    /// `PhysModelParseSolidByIndex` followed by `CreatePolyObject`'s template
    /// fill, minus the environment.
    ///
    /// `None` when the collision model has no solids at all, which is Valve's
    /// `if ( !pCollide || !pCollide->solidCount ) return NULL`.
    ///
    /// > **The `solid` block's `index` picks which solid**, and it is not
    /// > always zero: 36 of Portal 2's 1,056 collision models carry more than
    /// > one solid, because a jointed model has one per bone. Valve asserts
    /// > that the *first* block's index is 0 when no index was asked for; this
    /// > falls back to solid 0 rather than asserting, because a ragdoll whose
    /// > first block is not solid 0 should still produce a body.
    pub fn from_vcollide(collide: &VCollide) -> Option<Model> {
        let params = collide.solid_params();
        let solid = collide
            .solids
            .get(params.index)
            .or_else(|| collide.solids.first())?;
        Some(Model {
            hulls: Hulls::from_solid(solid),
            mass: Mass::from_solid(solid, &params),
            jointed: collide.solids.len() > 1,
            params,
        })
    }
}

#[cfg(test)]
mod depot {
    use super::*;
    use crate::engine::world::bsp::Bsp;
    use crate::engine::world::physics;
    use crate::engine::world::props::Props;
    use crate::filesystem::Vfs;
    use env::{Environment, Hulls, Motion};
    use glam::Vec3;

    fn game() -> (Vfs, std::path::PathBuf) {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");
        (vfs, dir)
    }

    fn walk(vfs: &Vfs, root: &str, suffix: &str) -> Vec<String> {
        let mut stack = vec![root.to_owned()];
        let mut out = Vec::new();
        while let Some(at) = stack.pop() {
            let Ok(entries) = vfs.list(&at) else { continue };
            for entry in entries {
                let child = format!("{at}/{}", entry.name);
                if entry.is_dir {
                    stack.push(child);
                } else if child.to_ascii_lowercase().ends_with(suffix) {
                    out.push(child);
                }
            }
        }
        out.sort();
        out
    }

    /// Every `.phy` Portal 2 ships, through the reader.
    ///
    /// The numbers here are the ones `portdocs/VPHYSICS.md` §2.6 was written
    /// from, and they were measured with an independent Python implementation
    /// of the same format before this one existed — so a mismatch means the
    /// Rust is wrong, not that the expectation has drifted.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release phy -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_collision_model_parses() {
        let (vfs, _) = game();
        let models = walk(&vfs, "models", ".mdl");
        let phys = walk(&vfs, "models", ".phy");
        assert!(
            models.len() > 1_000,
            "only {} models — is this a Portal 2 install?",
            models.len()
        );

        let (mut solids, mut ledges, mut points, mut triangles) = (0usize, 0usize, 0usize, 0usize);
        let (mut mixed, mut degenerate, mut multi_solid) = (0usize, 0usize, 0usize);
        let mut most_ledges = (0usize, String::new());
        for path in &phys {
            let bytes = vfs.read(path).expect("read a .phy");
            let collide = match collide::VCollide::read_phy(path, &bytes) {
                Ok(collide) => collide,
                Err(e) => panic!("{path}: {e}"),
            };
            if collide.solids.len() > 1 {
                multi_solid += 1;
            }
            solids += collide.solids.len();
            for solid in &collide.solids {
                ledges += solid.ledges.len();
                if solid.ledges.len() > most_ledges.0 {
                    most_ledges = (solid.ledges.len(), path.clone());
                }
                for ledge in &solid.ledges {
                    points += ledge.points.len();
                    triangles += ledge.triangles.len();
                    if ledge.mixed_materials {
                        mixed += 1;
                    }
                }
                degenerate += Hulls::from_solid(solid).degenerate;
            }
        }

        eprintln!(
            "{} models, {} with a .phy ({:.1}%); {solids} solid(s), {ledges} ledge(s), \
             {points} point(s), {triangles} triangle(s); {multi_solid} jointed; \
             most ledges {} in {}",
            models.len(),
            phys.len(),
            100.0 * phys.len() as f32 / models.len() as f32,
            most_ledges.0,
            most_ledges.1,
        );

        assert_eq!(models.len(), 2_041, "models the game ships");
        assert_eq!(phys.len(), 1_056, "…of which ship a collision model");
        assert_eq!(multi_solid, 51, "jointed models, one solid per bone");
        assert_eq!(ledges, 4_643, "convex pieces in all of them");
        assert_eq!(
            mixed, 0,
            "no *model* ledge mixes materials — the world's is the opposite case"
        );
        assert_eq!(
            degenerate, 0,
            "and every one of them makes a convex hull"
        );
        assert_eq!(most_ledges.0, 39);
        assert_eq!(most_ledges.1, "models/props_underground/elevator_enclosure.phy");
    }

    /// Every map's `LUMP_PHYSCOLLIDE`, and the two facts about the world that
    /// shape the whole module.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_map_carries_a_world_collision_model() {
        let (vfs, _) = game();
        let maps = walk(&vfs, "maps", ".bsp");
        assert!(maps.len() >= 100, "only {} maps", maps.len());

        let (mut models, mut world_ledges, mut mixed, mut virtual_terrain) = (0, 0usize, 0usize, 0);
        let mut most_models = (0usize, String::new());
        for path in &maps {
            let name = path
                .trim_start_matches("maps/")
                .trim_end_matches(".bsp")
                .to_owned();
            let bsp = Bsp::load(&vfs, &name).expect("load a map");
            assert!(
                !bsp.phys_collide.is_empty(),
                "{name} has no LUMP_PHYSCOLLIDE"
            );
            let lump = collide::VCollide::read_lump(&name, &bsp.phys_collide)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            models += lump.len();
            if lump.len() > most_models.0 {
                most_models = (lump.len(), name.clone());
            }
            for (index, collide) in &lump {
                if *index != 0 {
                    continue;
                }
                assert!(
                    collide.virtual_terrain(),
                    "{name}'s worldspawn does not say virtualterrain"
                );
                virtual_terrain += 1;
                for solid in &collide.solids {
                    world_ledges += solid.ledges.len();
                    mixed += solid.ledges.iter().filter(|l| l.mixed_materials).count();
                }
            }
        }

        eprintln!(
            "{} maps, {models} brush model(s), most {} in {}; \
             {world_ledges} world ledge(s), {mixed} mixing materials ({:.0}%)",
            maps.len(),
            most_models.0,
            most_models.1,
            100.0 * mixed as f32 / world_ledges as f32,
        );

        assert_eq!(maps.len(), 106);
        assert_eq!(virtual_terrain, 106, "every map defers its terrain");
        assert_eq!(models, 11_720, "brush models with collision, across the game");
        assert_eq!(most_models, (408, "mp_coop_start".to_owned()));
        assert_eq!(world_ledges, 115_225);
        // The measurement `portdocs/VPHYSICS.md` §3.5 rests on: two thirds of
        // the world's convex pieces name more than one surface material, so
        // there is no per-piece answer to pick and the world gets `default`.
        assert_eq!(mixed, 73_856);
    }

    /// The surface property database, read the way the game reads it.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_surface_property_database_resolves_the_cubes_chain() {
        let (vfs, _) = game();
        let props = physics::surface_properties(&vfs);
        eprintln!("{} surface properties", props.len());
        assert_eq!(props.len(), 92);

        // The cube's own, `Metal_Box` → `solidmetal`.
        let cube = props.resolve("Metal_Box");
        assert_eq!((cube.friction, cube.elasticity), (0.8, 0.1));
        // The reflective cube's, `reflective` → `metalpanel` → `metal`.
        let mirror = props.resolve("reflective");
        assert_eq!((mirror.friction, mirror.elasticity), (0.8, 0.2));
        // The world's.
        let default = props.resolve("default");
        assert_eq!((default.friction, default.elasticity), (0.8, 0.25));
        // The rule that is easy to get wrong: no `base`, no physics keys, and
        // still not zero.
        let silent = props.resolve("default_silent");
        assert_eq!((silent.friction, silent.elasticity), (0.8, 0.25));

        // How far the per-triangle world material this port cannot reproduce
        // could take friction: every material a shipped `materialtable` names
        // is 0.8 except `glass`.
        for name in [
            "default",
            "concrete",
            "wood",
            "default_silent",
            "metal",
            "plaster",
            "tile",
        ] {
            assert_eq!(props.resolve(name).friction, 0.8, "{name}");
        }
        assert_eq!(props.resolve("glass").friction, 0.5);

        // And the five surfaces whose elasticity is out of range, which is the
        // one place `CoefficientCombineRule::Multiply` cannot match IVP.
        let hot: Vec<&str> = props
            .iter()
            .filter(|s| s.elasticity > 1.0)
            .map(|s| s.name.as_str())
            .collect();
        eprintln!("superelastic surfaces: {hot:?}");
        assert_eq!(hot.len(), 5);
    }

    /// The control for the test below: the *shipped* cube hull, dropped from
    /// the same height onto a floor that is known to be flat.
    ///
    /// It exists because the map's answer has a 5.8° tilt in it and a reader
    /// deserves to know which end that came from. It came from the map.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_shipped_cube_hull_lands_flat_on_a_flat_floor() {
        let (vfs, _) = game();
        let bytes = vfs.read("models/props/metal_box.phy").expect("the cube");
        let phy = collide::VCollide::read_phy("metal_box.phy", &bytes).unwrap();
        let params = phy.solid_params();
        let solid = &phy.solids[params.index];

        let mut env = Environment::new(physics::surface_properties(&vfs));
        // A single wide slab, so that nothing about the *map* can be blamed.
        let floor: Vec<Vec3> = (0..8)
            .map(|i| {
                Vec3::new(
                    if i & 1 == 0 { -512.0 } else { 512.0 },
                    if i & 2 == 0 { -512.0 } else { 512.0 },
                    if i & 4 == 0 { -64.0 } else { 0.0 },
                )
            })
            .collect();
        let slab = collide::Solid {
            mass_center: Vec3::ZERO,
            rotation_inertia: Vec3::ONE,
            radius: 1024.0,
            ledges: vec![collide::Ledge {
                points: floor,
                triangles: Vec::new(),
                material: 0,
                mixed_materials: false,
            }],
        };
        env.add(
            Motion::Static,
            &Hulls::from_solid(&slab),
            Vec3::ZERO,
            Vec3::ZERO,
            "default",
            None,
        )
        .expect("a floor");

        let hulls = Hulls::from_solid(solid);
        let cube = env
            .add(
                Motion::Dynamic,
                &hulls,
                Vec3::new(0.0, 0.0, 255.0),
                Vec3::new(-0.336_266, 90.488_1, -0.912_995),
                &params.surface_prop,
                Some(env::Mass::from_solid(solid, &params)),
            )
            .expect("a cube");
        for _ in 0..320 {
            env.step();
        }
        let (origin, angles) = env.pose(cube).expect("still there");
        let (mins, maxs) = solid.bounds().expect("a hull");
        eprintln!(
            "flat floor: rest z {:.3} (half-height {:.3}), angles {angles:?}",
            origin.z,
            (maxs.z - mins.z) / 2.0
        );
        assert!(
            (origin.z - (maxs.z - mins.z) / 2.0).abs() < 0.5,
            "the cube should sit on the slab, not at {}",
            origin.z
        );
        // Tilt, measured as the angle between the cube's own up and the
        // world's — which is the thing a `QAngle` triple makes hard to read
        // when the yaw is 90°.
        let up = crate::math::angle_matrix(angles) * Vec3::Z;
        let tilt = up.z.clamp(-1.0, 1.0).acos().to_degrees();
        eprintln!("flat floor: tilt {tilt:.3}°");
        assert!(tilt < 1.0, "a cube dropped onto a flat floor rests flat, not {tilt}° off");
        assert!(!env.is_awake(cube));
    }

    /// The whole thing, on the map the port boots into: build the
    /// environment, drop the cube the map actually places, and watch it land.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_cube_on_sp_a1_intro1_falls_and_comes_to_rest() {
        let (vfs, _) = game();
        let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("load the map");
        let props = Props::load("sp_a1_intro1", &bsp).expect("the prop lump");
        let started = std::time::Instant::now();
        let mut built = physics::build(
            "sp_a1_intro1",
            &bsp,
            &props,
            &vfs,
            physics::surface_properties(&vfs),
        );
        built.add_models(&["models/props/metal_box.mdl".to_owned()], &vfs);
        let build_time = started.elapsed();
        eprintln!("{} in {:.0} ms", built.stats.summary(), build_time.as_secs_f32() * 1e3);
        // The map's entity-placed models, which in the running game get static
        // bodies from `Physics::add_studio_entities`. This test has no entity
        // list, so it drops the cube against the world and the static props
        // alone — a strictly emptier room than the game's.
        eprintln!("{:?}", built.stats);

        // Two of `sp_a1_intro1`'s four world solids are in a cube's way — the
        // level shell and a grate — and two are not: a playerclip and a
        // monsterclip. See `world::physics::add_world`.
        assert_eq!(built.stats.world_solids, 2);
        assert_eq!(built.stats.world_solids_skipped, 2);
        assert_eq!(built.stats.failed, 0, "{:?}", built.stats.first_failure);

        let model = built
            .models
            .get("models/props/metal_box.mdl")
            .expect("the cube's collision model")
            .clone();
        // The hull the shipped file describes: one convex piece, 40 kg.
        eprintln!(
            "cube: {} hull(s), {} kg, surfaceprop {:?}",
            model.hulls.len(),
            model.params.mass,
            model.params.surface_prop,
        );
        assert_eq!(model.params.mass, 40.0);
        assert_eq!(model.params.surface_prop, "Metal_Box");

        // Exactly where and how `sp_a1_intro1` places it — including the third
        // of a degree of tilt Hammer left on it.
        let origin = Vec3::new(-496.0, 4112.0, 2949.33);
        let angles = Vec3::new(-0.336_266, 90.488_1, -0.912_995);
        let mut env: Environment = built.environment;
        let cube = env
            .add(
                Motion::Dynamic,
                &model.hulls,
                origin,
                angles,
                &model.params.surface_prop,
                Some(model.mass),
            )
            .expect("a body for the cube");

        // Five seconds of game time. `physics_settled` is what a caller would
        // watch; here the tick count is fixed so the numbers are reproducible.
        let mut ticks_to_sleep = None;
        let stepping = std::time::Instant::now();
        for tick in 0..320 {
            env.step();
            if ticks_to_sleep.is_none() && !env.is_awake(cube) {
                ticks_to_sleep = Some(tick);
            }
        }
        let busy = stepping.elapsed();
        // …and again with everything asleep, which is what a map costs for
        // the other 99% of its life.
        let idle = std::time::Instant::now();
        for _ in 0..320 {
            env.step();
        }
        let idle = idle.elapsed();
        eprintln!(
            "step: {:.3} ms/tick while the cube is falling, {:.3} ms/tick once it sleeps",
            busy.as_secs_f32() * 1e3 / 320.0,
            idle.as_secs_f32() * 1e3 / 320.0,
        );
        let (rest, rest_angles) = env.pose(cube).expect("still there");
        let fell = origin.z - rest.z;
        let up = crate::math::angle_matrix(rest_angles) * Vec3::Z;
        let tilt = up.z.clamp(-1.0, 1.0).acos().to_degrees();
        eprintln!(
            "cube fell {fell:.2} units to z {:.2} (x {:.2} y {:.2}), \
             tilt {tilt:.2}°, asleep after {ticks_to_sleep:?} ticks",
            rest.z, rest.x, rest.y
        );

        assert!(
            ticks_to_sleep.is_some(),
            "the cube never came to rest in five seconds"
        );
        assert!(
            fell > 1.0,
            "the cube should have fallen onto something, moved {fell}"
        );
        assert!(
            (rest.x - origin.x).abs() < 32.0 && (rest.y - origin.y).abs() < 32.0,
            "and landed roughly under where it started, not at {rest:?}"
        );

        // Cross-check against the *other* collision representation of the
        // same map. `trace/` builds its own structures from the brush lumps
        // and has never seen `LUMP_PHYSCOLLIDE`; if the two disagree about
        // where the floor is, one of them is wrong.
        //
        // **This is also the answer to why the cube does not rest level.** The
        // floor of `sp_a1_intro1`'s first room is not flat — it slopes about
        // seven degrees — so a cube resting on it should not be flat either,
        // and `the_shipped_cube_hull_lands_flat_on_a_flat_floor` is the
        // control that says the tilt is the map's and not the solver's.
        use crate::engine::trace::{CollisionBsp, Contents, Ray};
        let collision = CollisionBsp::build(&bsp);
        let mut tracer = collision.tracer();
        let down = tracer.trace(
            &Ray::line(origin, origin - Vec3::Z * 512.0),
            Contents::MASK_SOLID,
        );
        let slope = down.normal.z.clamp(-1.0, 1.0).acos().to_degrees();
        let agreement = up.dot(down.normal).clamp(-1.0, 1.0).acos().to_degrees();
        eprintln!(
            "trace: floor at z {:.2}, normal {:?} ({slope:.2}° off vertical); \
             the cube's up is {agreement:.2}° from it",
            down.end.z, down.normal
        );

        assert!(
            (down.end.z - (rest.z - 17.79)).abs() < 1.0,
            "the body came to rest at {:.2} and the trace puts the floor at {:.2}",
            rest.z,
            down.end.z
        );
        assert!(
            agreement < 2.0,
            "a cube at rest should be lying on the floor it is resting on, \
             and its up is {agreement}° from the floor's normal"
        );
    }
}
