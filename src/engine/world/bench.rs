//! A headless measurement of what one frame of world drawing costs on the CPU.
//!
//! Not a test of behaviour — a stopwatch. It exists because the frame loop is
//! unmeasurable from outside the process: macOS stops delivering redraws to an
//! occluded window, so `sample`ing the running game shows a main thread parked
//! in `mach_msg` and nothing else. This opens a real device, loads a real map
//! and records real passes with no window in the way, so the cost of the
//! recording itself can be seen and optimised against.
//!
//! ```text
//! KISAK_GAME_DIR=/path/to/portal2 cargo test --release frame_cost -- --ignored --nocapture
//! ```
//!
//! What it measures is **CPU time spent recording a frame**, not frame rate:
//! there is no swap chain, so nothing waits for vsync and the number is the
//! part of the budget the engine actually controls.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use glam::Vec3;

    use crate::engine::world::World;
    use crate::filesystem::Vfs;
    use crate::materials::context::{Camera, Load, RenderContext};
    use crate::materials::target::RenderTarget;
    use crate::materials::MaterialCache;

    const SIZE: u32 = 640;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            eprintln!("skipping: adapter has no BC texture support");
            return None;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
            ..Default::default()
        }))
        .ok()
    }

    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn frame_cost() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let Some((device, queue)) = device() else {
            return;
        };
        let dir = PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut materials = MaterialCache::new(&device, &queue);
        let map = std::env::var("KISAK_MAP").unwrap_or_else(|_| "sp_a1_intro1".to_owned());
        let mut world = World::load(&vfs, &mut materials, &device, &map).expect("the map loads");
        println!("{}", world.summary());

        // **The models the game's entities place, which `World::load` cannot
        // read for itself** — they are named by the entity lump it has just
        // parsed, so they need a spawned entity list first. `Level::load` does
        // these two calls in this order and so does this; without them
        // `World::draw`'s fourth call records nothing and this stopwatch
        // silently stops measuring the largest thing in the frame.
        //
        // It went in with `prop_dynamic`: **`sp_a1_intro1` places 91 entity
        // models with 355,469 triangles**, against 1,080 static props with
        // 224,924.
        let mut server = crate::server::Server::new();
        server.level_init(&map, &world.entities, &world.models);
        // The map's `sky_camera`, if it has one — 7 of the game's 106 do.
        let sky3d = server.sky3d();
        let placements: Vec<crate::engine::world::entities::ModelEntity> = server
            .model_entities()
            .into_iter()
            .map(|e| crate::engine::world::entities::ModelEntity {
                id: e.id,
                model: e.model,
                origin: e.origin,
                angles: e.angles,
                skin: e.skin,
                visible: e.visible,
                sequence: e.sequence,
                cycle: e.cycle,
                anim_time: e.anim_time,
                playback_rate: e.playback_rate,
                modulation: e.modulation,
            })
            .collect();
        world.load_entity_models(&vfs, &mut materials, &device, &placements);
        println!("{}", world.entity_models.summary());

        // **A linked pair in front of the camera**, so that the recursive view
        // has something to recurse through. Placed in open air rather than on
        // a wall for the reason `portalview`'s rendered test gives: a trace out
        // of this spawn goes through the container the player wakes up in, and
        // a portal behind an opaque surface costs nothing to draw.
        //
        // Both at one point, sixty degrees apart — the angle is what decides
        // whether the *second* level has anything to draw, and this is the
        // case that costs the most.
        let bench_eye = world
            .spawn
            .map(|spawn| spawn.origin + Vec3::Z * 64.0)
            .unwrap_or_else(|| world.center());
        let portal = |id: u64, yaw: f32, is_portal2: bool, matrix: glam::Mat4| {
            crate::engine::world::portals::Portal {
                id,
                origin: bench_eye + Vec3::X * 96.0,
                angles: Vec3::new(0.0, yaw, 0.0),
                half_width: 32.0,
                half_height: 54.0,
                is_portal2,
                open_for: 10.0,
                static_for: 10.0,
                linked: Some(1 - id),
                matrix,
            }
        };
        let (entrance, exit) = (
            (bench_eye + Vec3::X * 96.0, Vec3::new(0.0, 180.0, 0.0)),
            (bench_eye + Vec3::X * 96.0, Vec3::new(0.0, -60.0, 0.0)),
        );
        use crate::server::classes::portal::teleport_matrix;
        world.sync_portals(&[
            portal(0, 180.0, false, teleport_matrix(entrance, exit)),
            portal(1, -60.0, true, teleport_matrix(exit, entrance)),
        ]);
        let world = world;

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let target = RenderTarget::new(
            &device,
            "bench",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );
        // **The map's own spawn, not the centre of its bounding box.** The
        // centre of a map is usually inside its geometry or in a sealed
        // pocket, and a frame recorded from there measures nothing: the first
        // run of this after visibility landed reported *one leaf* and 53
        // faces, because `sp_a1_intro1`'s box centre is in an area with no
        // areaportals into it.
        let eye = world
            .spawn
            .map(|spawn| spawn.origin + Vec3::Z * 64.0)
            .unwrap_or_else(|| world.center());
        let camera = Camera::perspective(
            eye,
            glam::camera::rh::view::look_to_mat4(eye, Vec3::X, Vec3::Z),
            90.0,
            1.0,
            7.0,
            28_400.0,
        );

        const FRAMES: u32 = 120;
        let mut run = |what: &str, draw: &dyn Fn(&mut crate::materials::context::Pass<'_>)| {
            let frame = |context: &mut RenderContext, materials: &mut MaterialCache| {
                context.begin_frame();
                let mut encoder = device.create_command_encoder(&Default::default());
                {
                    let mut pass = context.offscreen_pass(
                        &mut encoder,
                        materials.pipelines(),
                        &target,
                        &camera,
                        Load::Clear(wgpu::Color::BLACK),
                    );
                    draw(&mut pass);
                }
                queue.submit([encoder.finish()]);
            };
            // One warm frame: the first builds every pipeline the map needs,
            // which is a load cost and not a frame cost.
            frame(&mut context, &mut materials);
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("idle");

            let start = Instant::now();
            for _ in 0..FRAMES {
                frame(&mut context, &mut materials);
            }
            let recorded = start.elapsed();
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("idle");
            let total = start.elapsed();
            println!(
                "  {what:<16} {:>7.2} ms/frame CPU  ({:>6.2} ms with GPU wait, \
                 {:>5.0} fps ceiling from CPU alone)",
                recorded.as_secs_f64() * 1000.0 / f64::from(FRAMES),
                total.as_secs_f64() * 1000.0 / f64::from(FRAMES),
                f64::from(FRAMES) / recorded.as_secs_f64(),
            );
        };

        // What this view can actually see. Timed here rather than inside the
        // frame closures because it runs **once** per frame for every pass,
        // and folding it into one of them would charge that pass for all of
        // them.
        let start = Instant::now();
        for _ in 0..FRAMES {
            std::hint::black_box(world.visible(eye, camera.view_proj(), false));
        }
        let marking = start.elapsed();
        let visible = world.visible(eye, camera.view_proj(), false);
        let everything = crate::engine::world::vis::VisibleSet::everything();
        let s = visible.stats;
        println!("{map}:");
        println!(
            "  {:<16} {:>7.3} ms/frame CPU  (cluster {}, {} of {} clusters, {} leaves, \
             {} faces, {} areas, {} nodes)",
            "visibility",
            marking.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            s.cluster,
            s.clusters,
            world.vis.cluster_count(),
            s.leaves,
            s.faces,
            s.areas,
            s.nodes,
        );
        run("brushes only", &|pass| world.draw_brushes(pass, &visible));
        run("brush models", &|pass| {
            world.draw_brush_models(pass, &visible)
        });
        run("props only", &|pass| {
            world.prop_models.draw(pass, &world.props, &visible)
        });
        run("entity models", &|pass| {
            world.entity_models.draw(pass, 0.0, &|mins, maxs| {
                world.box_visible(&visible, mins, maxs)
            })
        });
        run("everything", &|pass| world.draw(pass, 0.0, &visible));
        // **The 3D skybox's whole pass**, on the maps that have one — the box
        // and the room behind it, from the sky camera.
        //
        // Measured **unconditionally**, where the running engine asks three
        // questions first: `sp_a1_intro1`'s spawn is inside the sealed
        // container the player wakes up in, whose leaf does not claim to see
        // the sky, so a benchmark that reproduced the gate would print a zero
        // and measure nothing. What this answers is "what does a sky view cost
        // when there is one", which is the number that matters for the maps
        // and viewpoints where it draws.
        if let Some(sky3d) = sky3d {
            let sky_camera = Camera::perspective(
                sky3d.eye(eye),
                glam::camera::rh::view::look_to_mat4(sky3d.eye(eye), Vec3::X, Vec3::Z),
                90.0,
                1.0,
                crate::engine::world::sky::SKY_ZNEAR,
                crate::engine::world::sky::SKY_ZFAR,
            );
            let sky_visible = world.sky_visible_set(&sky_camera, &sky3d, false);
            let draw_box = world.sky_visible(&sky_visible);
            println!(
                "  {:<16} {} faces, {} clusters, box {}",
                "sky view sees",
                sky_visible.stats.faces,
                sky_visible.stats.clusters,
                draw_box,
            );
            run("3d skybox", &|pass| {
                world.draw_sky_view(pass, 0.0, &sky_camera, &sky_visible, draw_box)
            });
        }
        // The same frame with the PVS off, which is what every measurement in
        // `rustdocs/ENGINE.md` before this was: the number to compare against.
        run("everything, novis", &|pass| {
            world.draw(pass, 0.0, &everything)
        });
        // **The recursive view, at each depth the cvar allows by default.**
        // Whole frames including the world, so the number to read is the
        // difference from `everything`: one level is one more world draw from
        // somewhere else, and the marginal cost of the second is what says
        // whether the rectangle narrowing and the PVS are doing their job.
        for depth in 1..=crate::engine::world::portalview::DEFAULT_RECURSION {
            let setup = crate::engine::world::portalview::PortalViewSetup {
                curtime: 0.0,
                max_depth: depth,
                viewport: (SIZE, SIZE),
                novis: false,
            };
            run(&format!("+ portal depth {depth}"), &|pass| {
                world.draw(pass, 0.0, &visible);
                world.draw_portal_views(pass, &setup, &camera, &visible);
            });
        }
        drop(run);

        // The translucent half, which is a *third* pass and — unlike the two
        // above — records one draw per instance rather than one per batch,
        // because a back-to-front order is what a sort costs. Measured
        // separately for that reason: the number to watch is how the per-item
        // cost compares with the batched passes, not the absolute.
        {
            let list = world.translucent_list(eye, Vec3::X, &visible);
            println!("  {:<16} {} draws", "translucent", list.len());
            let translucent_frame = |context: &mut RenderContext, materials: &mut MaterialCache| {
                context.begin_frame();
                let mut encoder = device.create_command_encoder(&Default::default());
                {
                    let mut pass = context.offscreen_pass(
                        &mut encoder,
                        materials.pipelines(),
                        &target,
                        &camera,
                        Load::Clear(wgpu::Color::BLACK),
                    );
                    world.draw_translucent(&mut pass, 0.0, &list, &visible, 2);
                }
                queue.submit([encoder.finish()]);
            };
            translucent_frame(&mut context, &mut materials);
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("idle");
            let start = Instant::now();
            for _ in 0..FRAMES {
                translucent_frame(&mut context, &mut materials);
            }
            let recorded = start.elapsed();
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("idle");
            let total = start.elapsed();
            println!(
                "  {:<16} {:>7.2} ms/frame CPU  ({:>6.2} ms with GPU wait, \
                 {:>5.0} fps ceiling from CPU alone)",
                "translucent",
                recorded.as_secs_f64() * 1000.0 / f64::from(FRAMES),
                total.as_secs_f64() * 1000.0 / f64::from(FRAMES),
                f64::from(FRAMES) / recorded.as_secs_f64(),
            );
        }

        // The refracting half, which is a *second* pass with a full-screen copy
        // in front of it — so it cannot be measured through `run` above, which
        // records one pass. Reported separately for the same reason
        // `engine::exposure` reports the tone mapper's two passes separately:
        // the question is what the frame-buffer copy and the extra pass cost
        // against the draw they sit around.
        if !world.needs_frame_buffer_copy() {
            println!("  {:<16} nothing refracts on this map", "refractors");
            return;
        }
        let refract_frame = |context: &mut RenderContext, materials: &mut MaterialCache| {
            context.begin_frame();
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    &target,
                    &camera,
                    Load::Clear(wgpu::Color::BLACK),
                );
                world.draw(&mut pass, 0.0, &visible);
            }
            context.record_refract_texture(&mut encoder, &target);
            {
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    &target,
                    &camera,
                    Load::Keep,
                );
                world.draw_refracting(&mut pass, 0.0, &visible);
            }
            queue.submit([encoder.finish()]);
        };
        refract_frame(&mut context, &mut materials);
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("idle");
        let start = Instant::now();
        for _ in 0..FRAMES {
            refract_frame(&mut context, &mut materials);
        }
        let recorded = start.elapsed();
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("idle");
        let total = start.elapsed();
        println!(
            "  {:<16} {:>7.2} ms/frame CPU  ({:>6.2} ms with GPU wait, \
             {:>5.0} fps ceiling from CPU alone)",
            "+ refractors",
            recorded.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            total.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            f64::from(FRAMES) / recorded.as_secs_f64(),
        );
    }
}
