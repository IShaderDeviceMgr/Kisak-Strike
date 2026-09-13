//! A headless measurement of what the tone mapper does to a real map.
//!
//! The sibling of [`world::bench`](super::world::bench): depot-gated, no window,
//! real content. `bench` answers "what does a frame cost"; this answers "where
//! does the exposure settle, and what does the picture look like when it gets
//! there" — which is the only question about an exposure controller that
//! matters and the only one a synthetic histogram cannot answer.
//!
//! ```text
//! KISAK_GAME_DIR=/path/to/portal2 cargo test --release exposure -- --ignored --nocapture
//! ```
//!
//! `KISAK_MAP` picks the map (`sp_a1_intro1` by default) and
//! `KISAK_AUTOEXPOSURE_MAX` raises the exposure ceiling, which is how to ask
//! what a map would look like under the limit its own `env_tonemap_controller`
//! sets — 105 of the game's 106 maps have one and none of them uses the cvar
//! default (`portdocs/CLIENT_TONEMAP.md` §6).
//!
//! It runs the whole loop the game runs: draw the world into
//! [`PostProcess`](crate::materials::PostProcess)'s scene target with the
//! exposure the controller chose, measure that target, feed the counts back,
//! repeat. The only things missing relative to the running game are the swap
//! chain and the UI, neither of which is measured.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use crate::client::tonemap::{bucket_bounds, BUCKETS};
    use crate::client::Client;
    use crate::engine::console::{Console, NoConfigFiles};
    use crate::engine::world::World;
    use crate::filesystem::Vfs;
    use crate::materials::context::{Camera, Load, RenderContext};
    use crate::materials::histogram::Region;
    use crate::materials::pipeline::TargetFormat;
    use crate::materials::target::{RenderTarget, DEPTH_FORMAT};
    use crate::materials::{MaterialCache, PostProcess};

    /// The back buffer this pretends to have. `Bgra8UnormSrgb` is what the
    /// surface actually reports on macOS, and the format is load-bearing here:
    /// the histogram reads linear light out of an sRGB target, so measuring a
    /// linear one would be measuring a different picture.
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
    const WIDTH: u32 = 1280;
    const HEIGHT: u32 = 720;
    /// 60 fps for two seconds, which is well past the controller's own
    /// settling time.
    const FRAMES: u32 = 120;
    const DT: f32 = 1.0 / 60.0;

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
    fn exposure_settles_on_a_real_map() {
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

        // A real client, spawned where the map spawns one, so that the camera
        // is the camera a player gets rather than a guess. The tone mapper is
        // the client's, which is also what the running engine drives.
        let mut console = Console::new(Box::new(NoConfigFiles), Vec::new());
        let mut client = Client::new(&mut console);
        // The shipped maps set their own ceiling through an
        // `env_tonemap_controller` and there are no entities, so the cvar
        // default of 2 is what applies here — and no shipped map uses it
        // (`portdocs/CLIENT_TONEMAP.md` §6). This is how to ask what the map
        // would look like under the ceiling it actually asks for.
        if let Ok(max) = std::env::var("KISAK_AUTOEXPOSURE_MAX") {
            console
                .cvars()
                .find("mat_autoexposure_max")
                .expect("registered")
                .set_string(&max);
        }
        match world.spawn {
            Some(spawn) => client.spawn(spawn.origin, spawn.pitch, spawn.yaw),
            None => client.spawn(world.center(), 0.0, 0.0),
        }
        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let mut post = PostProcess::new(
            &device,
            &queue,
            TargetFormat {
                color: FORMAT,
                depth: Some(DEPTH_FORMAT),
                samples: 1,
            },
            &bucket_bounds(),
        );

        // `Engine::camera`, verbatim: what the player at the spawn point sees.
        let view = client.view(WIDTH, HEIGHT);
        let (forward, _, up) = view.angles.vectors();
        let camera = Camera::perspective(
            view.origin,
            glam::camera::rh::view::look_at_mat4(view.origin, view.origin + forward, up),
            view.fov,
            view.aspect,
            view.z_near,
            view.z_far,
        );
        println!(
            "eye ({:.0} {:.0} {:.0}) pitch {:.0} yaw {:.0}",
            view.origin.x, view.origin.y, view.origin.z, view.angles.pitch, view.angles.yaw
        );

        // Stands in for the swap-chain image. Nothing reads it; the point is
        // that the presenting pass runs, because it is part of the frame cost
        // and part of what could go wrong.
        let back = RenderTarget::new(&device, "back buffer", WIDTH, HEIGHT, FORMAT, false);

        let unexposed = client.tonemap().current();
        let mut measurements = 0;
        // CPU time spent recording the two passes this module added, separately
        // from the world draw they sit around — `world::bench` measures that,
        // and the question here is what auto exposure costs on top of it.
        let mut scene_cpu = std::time::Duration::ZERO;
        let mut post_cpu = std::time::Duration::ZERO;
        for _ in 0..FRAMES {
            context.begin_frame();
            if let Some(counts) = post.measurement() {
                client.tonemap_mut().measured(counts.as_slice(), DT);
                measurements += 1;
            }
            context.set_exposure(client.tonemap_mut().scale());

            let mut encoder = device.create_command_encoder(&Default::default());
            let started = Instant::now();
            {
                let scene = post.scene((WIDTH, HEIGHT));
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    scene,
                    &camera,
                    Load::Clear(wgpu::Color::BLACK),
                );
                world.draw(&mut pass);
            }
            let drawn = Instant::now();
            post.record(
                &mut encoder,
                back.view(),
                Some(client.tonemap().exposure_region()),
            );
            scene_cpu += drawn - started;
            post_cpu += started.elapsed() - (drawn - started);
            queue.submit([encoder.finish()]);
            // The frame loop's `present`. **Waited on here and polled in the
            // game**: a vsynced frame gives the GPU a whole refresh to finish
            // in, and a test submitting flat out would otherwise run ahead of
            // both staging buffers and skip most of its own measurements.
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }

        let tonemap = client.tonemap();

        let (min, max) = tonemap.exposure_range();
        let (actual, wanted) = tonemap.bright_end().expect("something was measured");
        println!("{map} at {WIDTH}x{HEIGHT}, {FRAMES} frames:");
        println!("  measurements folded in:  {measurements}");
        println!(
            "  CPU per frame:           {:.2} ms scene, {:.3} ms measure + present",
            scene_cpu.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            post_cpu.as_secs_f64() * 1000.0 / f64::from(FRAMES),
        );
        println!(
            "  exposure:                {unexposed:.3} -> {:.3} (target {:.3}, allowed {min:.2}..{max:.2})",
            tonemap.current(),
            tonemap.target(),
        );
        println!(
            "  bright end:              {:.1}% of range, wants {:.1}%",
            actual * 100.0,
            wanted * 100.0
        );
        println!(
            "  median luminance:        {:.2}%",
            tonemap.median_luminance().unwrap_or(0.0) * 100.0
        );
        let counts = tonemap.histogram();
        let total: u32 = counts.iter().sum();
        let bounds = bucket_bounds();
        for (i, &count) in counts.iter().enumerate() {
            let share = count as f32 / total.max(1) as f32;
            println!(
                "  {:5.3}..{:5.3} {:6.2}% {}",
                bounds[i],
                bounds[i + 1],
                share * 100.0,
                "#".repeat((share * 60.0).round() as usize)
            );
        }

        // The loop ran: a measurement every frame once the two staging buffers
        // are in rotation, minus the couple it takes to fill them.
        assert!(
            measurements >= FRAMES as usize - 4,
            "only {measurements} of {FRAMES} frames produced a measurement"
        );
        // Every pixel of the measured rectangle is accounted for exactly once
        // — the half-open buckets are what make that true.
        let (fraction_x, fraction_y) = tonemap.exposure_region();
        let region = Region::centered(WIDTH, HEIGHT, fraction_x, fraction_y);
        assert_eq!(total, region.width * region.height);
        // It settled somewhere legal, and somewhere other than where it
        // started — a map this port draws unexposed is measurably too dark, so
        // an exposure that stayed at 1.0 would mean the loop is not closed.
        assert!(tonemap.current() >= min && tonemap.current() <= max);
        assert_ne!(tonemap.current(), unexposed);
        assert_eq!(counts.len(), BUCKETS);
    }
}
