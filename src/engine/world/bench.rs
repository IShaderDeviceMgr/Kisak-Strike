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
        let world = World::load(&vfs, &mut materials, &device, &map).expect("the map loads");
        println!("{}", world.summary());

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let target = RenderTarget::new(
            &device,
            "bench",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );
        let eye = world.center();
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

        println!("{map}:");
        run("brushes only", &|pass| world.draw_brushes(pass));
        run("brush models", &|pass| world.draw_brush_models(pass));
        run("props only", &|pass| {
            world.prop_models.draw(pass, &world.props)
        });
        run("everything", &|pass| world.draw(pass, 0.0));
        drop(run);

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
                world.draw(&mut pass, 0.0);
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
                world.draw_refracting(&mut pass, 0.0);
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
