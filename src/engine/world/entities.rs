//! Studio models that an **entity** places, posed by their animation.
//!
//! The third and last kind of geometry in a level shell. World faces and brush
//! entities are `.bsp` geometry drawn with a matrix; static props are `.mdl`
//! geometry the *map compiler* placed, in the `sprp` lump, never moving and lit
//! once at load. This is `.mdl` geometry the **game** places: where it is and
//! what it is doing are the server's to say, and it can change every tick.
//!
//! In Valve this is `C_BaseAnimating` and the client's renderable list. Here it
//! is one struct, because the port has exactly one animated entity class —
//! `prop_floor_button` — and the machinery a second one needs is already all
//! here.
//!
//! # The join with the game is a name, and the pose is the engine's
//!
//! The server says **which sequence** an entity is playing and **when it
//! started** ([`ModelEntity`]); the engine looks the sequence up in the model
//! by name and works out the cycle from the scene clock. That split is Valve's
//! own: `CBaseAnimating` networks `m_nSequence` and `m_flAnimTime` and it is
//! `C_BaseAnimating::FrameAdvance` on the *client* that turns them into a
//! cycle, because the server ticks at 64 Hz and animation wants to be smooth.
//!
//! It also keeps `server/`'s promise that it names no studio and no GPU type:
//! what crosses the boundary is a `&'static str`, three floats and a model
//! path.
//!
//! # Posing without skinning
//!
//! A model is drawn one **bone run** at a time — see
//! [`StudioModel::rigid_bones`](crate::studio::StudioModel::rigid_bones). Each
//! batch's triangles were sorted by bone at load, so each bone's are a
//! contiguous index range that can be drawn under that bone's own matrix, and
//! the vertex format, the shaders and the bind groups are untouched. For a
//! `prop_floor_button` that is two draws where a static prop is one: 7,263
//! triangles' worth of body under the identity and 666 of plate under whatever
//! the animation says.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};

use super::props::light::AmbientLighting;
use super::props::models::{PropBatch, PropModel};
use crate::engine::trace::CollisionBsp;
use crate::filesystem::Vfs;
use crate::materials::context::Pass;
use crate::materials::mesh::{StaticLightVertex, VertexBuffer, VertexLayout};
use crate::materials::uniforms::ModelLighting;
use crate::materials::{Material, MaterialCache};
use crate::studio::StudioModel;

/// One model-drawing entity, as the game server describes it.
///
/// `world/` names no server type and `server/` names no studio type, so this
/// is the whole vocabulary between them — the same arrangement
/// [`Placement`](super::Placement) already has for brush entities.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntity {
    /// The model path, as [`StudioModel::load`] wants it.
    pub model: String,
    pub origin: Vec3,
    /// Pitch, yaw, roll.
    pub angles: Vec3,
    /// `m_nSkin`. Not read yet — a model's skin families are
    /// `portdocs/STUDIO.md` stage 6's — and carried so that the seam does not
    /// have to change when they are.
    pub skin: i32,
    /// The sequence's **label**, as `LookupSequence` takes it: `"up"`,
    /// `"down"`. An empty string, or one the model does not have, is the bind
    /// pose.
    pub sequence: &'static str,
    /// `m_flAnimTime` — the scene time the sequence was reset at, which is
    /// what the cycle is measured from.
    pub anim_time: f32,
}

/// One placed instance, resolved against a loaded model.
struct Instance {
    /// Index into [`EntityModels::models`].
    model: usize,
    /// `model_to_world` — the entity's placement, which every bone's matrix is
    /// composed onto the left of.
    transform: Mat4,
    /// The sequence index this model has for the entity's label, or `None` for
    /// the bind pose.
    sequence: Option<usize>,
    anim_time: f32,
    lighting: ModelLighting,
}

/// What loading a map's entity models cost.
#[derive(Debug, Clone, Default)]
pub struct EntityModelStats {
    /// Entities that name a model and were given one.
    pub instances: usize,
    /// Distinct models uploaded.
    pub models: usize,
    /// Models that would not load; their entities draw nothing.
    pub models_missing: usize,
    pub triangles: usize,
    /// Instances whose model has more than one bone — the ones the pose is
    /// actually for.
    pub animated: usize,
    /// Models whose vertices are **not** each bound to exactly one bone, and
    /// which are therefore drawn in their bind pose rather than wrongly. See
    /// [`StudioModel::rigid_bones`](crate::studio::StudioModel::rigid_bones).
    pub models_not_rigid: usize,
}

/// Every studio model the map's entities place, uploaded and placed.
#[derive(Default)]
pub struct EntityModels {
    models: Vec<PropModel>,
    instances: Vec<Instance>,
    /// The black colour stream every instance binds in slot 1.
    ///
    /// An entity model has no `.vhv` — `vrad` bakes per-vertex lighting for
    /// *static* props and nothing else — so every one of these is lit by its
    /// ambient cube alone and `ModelLighting::static_light` stays 0. Something
    /// must still be bound, because the vertex layout says so.
    unlit: Option<VertexBuffer>,
    refracts: bool,
    pub stats: EntityModelStats,
}

impl EntityModels {
    /// Reads, uploads and places every model the given entities name.
    ///
    /// **Cannot fail**, for the same reason `PropModels::load` cannot: a model
    /// that will not load is an entity that does not draw, and the shipped
    /// engine logs and carries on.
    ///
    /// `ambient` and `collision` light each instance where it stands. That
    /// happens **once, here** — a `prop_floor_button` does not move, and
    /// nothing that does yet draws a model — so the condition for resampling
    /// per frame is the first entity model that travels.
    pub fn load(
        vfs: &Vfs,
        materials: &mut MaterialCache,
        device: &wgpu::Device,
        entities: &[ModelEntity],
        ambient: &AmbientLighting,
        collision: &CollisionBsp,
    ) -> EntityModels {
        let mut stats = EntityModelStats::default();
        let mut models: Vec<PropModel> = Vec::new();
        let mut by_name: HashMap<String, Option<usize>> = HashMap::new();
        let mut resolved: HashMap<String, Arc<Material>> = HashMap::new();
        let error = materials.error_model_material();
        let mut instances = Vec::new();

        for entity in entities {
            let key = entity.model.to_ascii_lowercase();
            let slot = *by_name.entry(key).or_insert_with(|| {
                let model = match StudioModel::load(vfs, &entity.model) {
                    Ok(model) => model,
                    Err(e) => {
                        eprintln!("source-engine: entity models: {e}");
                        stats.models_missing += 1;
                        return None;
                    }
                };
                if model.vertices.is_empty() || model.indices.is_empty() {
                    stats.models_missing += 1;
                    return None;
                }
                if model.bones.len() > 1 && model.rigid_bones().is_none() {
                    // Drawn in its bind pose rather than wrongly: the per-bone
                    // split cannot express a vertex two bones share. No model
                    // the port loads reaches this.
                    eprintln!(
                        "source-engine: entity models: {}: vertices are shared between bones; \
                         drawing it unanimated",
                        model.path
                    );
                    stats.models_not_rigid += 1;
                }

                let batches: Vec<PropBatch> = model
                    .batches
                    .iter()
                    .map(|batch| PropBatch {
                        material: resolve_material(
                            vfs,
                            materials,
                            &mut resolved,
                            &error,
                            &batch.material,
                        ),
                        first_index: batch.first_index,
                        index_count: batch.index_count,
                        bones: batch.bones.clone(),
                    })
                    .collect();

                stats.models += 1;
                stats.triangles += model.indices.len() / 3;
                models.push(PropModel::upload(device, model, batches));
                Some(models.len() - 1)
            });

            let Some(slot) = slot else { continue };
            let model = &models[slot];
            stats.instances += 1;
            if model.bones.len() > 1 {
                stats.animated += 1;
            }
            instances.push(Instance {
                model: slot,
                transform: Mat4::from_translation(entity.origin)
                    * Mat4::from_mat3(crate::math::angle_matrix(entity.angles)),
                sequence: model.sequence(entity.sequence),
                anim_time: entity.anim_time,
                lighting: ambient.lighting_at(collision, entity.origin),
            });
        }

        let widest = models.iter().map(|m| m.vertex_count).max().unwrap_or(0);
        let refracts = instances.iter().any(|instance| {
            models[instance.model]
                .batches
                .iter()
                .any(|batch| batch.material.needs_frame_buffer_copy)
        });

        EntityModels {
            unlit: (widest > 0).then(|| {
                VertexBuffer::new(
                    device,
                    "entity model lighting (none)",
                    &vec![StaticLightVertex::UNLIT; widest],
                )
            }),
            models,
            instances,
            refracts,
            stats,
        }
    }

    /// Takes each entity's sequence and start time from whoever owns them —
    /// the game server — once a frame.
    ///
    /// The list is positional: the `n`th [`ModelEntity`] here is the `n`th that
    /// [`load`](EntityModels::load) was given, which holds because nothing
    /// creates or destroys a model entity after the spawn pass. A shorter or
    /// longer list is ignored past the overlap rather than being an error, the
    /// way [`sync_brush_models`](super::World::sync_brush_models)'s `None` is.
    ///
    /// **The placement is taken too**, so that an entity model on a moving
    /// platform follows it — its *lighting* does not, which is the limitation
    /// [`load`](EntityModels::load) records.
    pub fn sync(&mut self, entities: &[ModelEntity]) {
        for (instance, entity) in self.instances.iter_mut().zip(entities) {
            instance.transform = Mat4::from_translation(entity.origin)
                * Mat4::from_mat3(crate::math::angle_matrix(entity.angles));
            instance.sequence = self.models[instance.model].sequence(entity.sequence);
            instance.anim_time = entity.anim_time;
        }
    }

    /// A one-line summary for the startup log.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        let mut out = format!(
            "{} entity models ({} models, {} triangles, {} animated)",
            s.instances, s.models, s.triangles, s.animated
        );
        if s.models_missing > 0 {
            out += &format!(", {} missing", s.models_missing);
        }
        if s.models_not_rigid > 0 {
            out += &format!(", {} not rigid (drawn unanimated)", s.models_not_rigid);
        }
        out
    }

    /// Whether any of them wears a material that reads the frame it is drawn
    /// into.
    pub fn refracts(&self) -> bool {
        self.refracts
    }

    /// Records the opaque half. `curtime` is the scene clock, which is what
    /// each instance's cycle is measured against.
    pub fn draw(&self, pass: &mut Pass<'_>, curtime: f32) {
        self.record(pass, curtime, false);
    }

    /// Records the half whose material samples a copy of the scene — see
    /// [`World::draw_refracting`](super::World::draw_refracting).
    pub fn draw_refracting(&self, pass: &mut Pass<'_>, curtime: f32) {
        if self.refracts {
            self.record(pass, curtime, true);
        }
    }

    fn record(&self, pass: &mut Pass<'_>, curtime: f32, refracting: bool) {
        for instance in &self.instances {
            let model = &self.models[instance.model];
            let Some(unlit) = self
                .unlit
                .as_ref()
                .map(|buffer| buffer.range(0, model.vertex_count as u32))
            else {
                continue;
            };

            // One pose per instance per frame: three `Mat4`s for a button.
            // Taken before the batch loop because every batch of one instance
            // shares it.
            let pose = model.pose(instance.sequence.unwrap_or(usize::MAX), self.cycle(instance, curtime));

            pass.set_model_lighting(&instance.lighting);
            pass.bind_static_light(&unlit);
            let vertices = model.vertices.slice();

            for batch in &model.batches {
                if batch.material.needs_frame_buffer_copy != refracting {
                    continue;
                }
                for run in &batch.bones {
                    let bone = pose.get(usize::from(run.bone)).copied().unwrap_or(Mat4::IDENTITY);
                    let indices = model.indices.range(run.first_index, run.index_count);
                    pass.draw_modulated(
                        &batch.material,
                        &vertices,
                        &indices,
                        instance.transform * bone,
                        [1.0; 4],
                    );
                }
            }
        }
    }

    /// `C_BaseAnimating::FrameAdvance` reduced to what a non-looping sequence
    /// needs: how far through the animation we are, from how long it has been
    /// playing.
    ///
    /// > **A sequence that is not `STUDIO_LOOPING` clamps at 1 and stays
    /// > there.** That is what makes a floor button *stay* pressed: `down` is
    /// > 11 frames at 24 fps, and at 0.42 seconds the plate has arrived and
    /// > the pose stops changing. All four of the button models' sequences are
    /// > `STUDIO_NOFORCELOOP`, and a looping one wraps instead.
    fn cycle(&self, instance: &Instance, curtime: f32) -> f32 {
        let model = &self.models[instance.model];
        let Some(sequence) = instance.sequence else {
            return 0.0;
        };
        let Some(anim) = model.animation(sequence) else {
            return 0.0;
        };
        let duration = anim.duration();
        if duration <= 0.0 {
            return 0.0;
        }
        let elapsed = (curtime - instance.anim_time).max(0.0);
        let cycle = elapsed / duration;
        match model.sequences[sequence].flags & crate::studio::anim::STUDIO_LOOPING != 0 {
            true => cycle.fract(),
            false => cycle.min(1.0),
        }
    }
}

/// The material a batch wears, refusing one whose shader cannot take model
/// geometry.
///
/// The same rule `PropModels::load` applies, and for the same reason: a prop's
/// geometry is `ModelVertex` and nothing else, so a brush shader on it would be
/// a validation failure rather than a wrong picture.
fn resolve_material(
    vfs: &Vfs,
    materials: &mut MaterialCache,
    resolved: &mut HashMap<String, Arc<Material>>,
    error: &Arc<Material>,
    name: &str,
) -> Arc<Material> {
    resolved
        .entry(name.to_owned())
        .or_insert_with(|| {
            let material = materials.load(vfs, name);
            if Arc::ptr_eq(&material, &materials.error_material())
                || material.shader.vertex_layout() != VertexLayout::Model
            {
                return Arc::clone(error);
            }
            material
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::context::{Camera, Load, RenderContext};
    use crate::materials::target::RenderTarget;
    use crate::server::Server;

    const SIZE: u32 = 256;

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

    /// **The floor button, drawn — and drawn differently as it presses.**
    ///
    /// The one test that can say the whole path works, because every step of
    /// it is invisible on its own: the `.mdl` could load and never reach a
    /// pass, the bone runs could be drawn under the wrong matrices, the pose
    /// could be composed on the wrong side of the placement, or the material
    /// could have fallen back to the error checkerboard. All of those draw
    /// *something*; only one of them draws something that **changes when the
    /// animation does**.
    ///
    /// It renders `sp_a1_intro1`'s button from a camera a few feet in front of
    /// it, at the two ends of `down`, and compares the images.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release the_button_draws -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install and a GPU; set KISAK_GAME_DIR"]
    fn the_button_draws_and_moves_as_it_presses() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let Some((device, queue)) = device() else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let mut materials = MaterialCache::new(&device, &queue);
        let map = std::env::var("KISAK_MAP").unwrap_or_else(|_| "sp_a1_intro1".to_owned());
        let mut world = super::super::World::load(&vfs, &mut materials, &device, &map)
            .expect("the map loads");

        // The game half: spawn the entities, then ask them what models they
        // place — the same two calls `Level::load` makes.
        let mut server = Server::new();
        server.level_init(&map, &world.entities, &world.models);
        let placements: Vec<ModelEntity> = server
            .model_entities()
            .into_iter()
            .map(|e| ModelEntity {
                model: e.model,
                origin: e.origin,
                angles: e.angles,
                skin: e.skin,
                sequence: e.sequence,
                anim_time: e.anim_time,
            })
            .collect();
        assert!(
            !placements.is_empty(),
            "{map} places no model entity; try KISAK_MAP=sp_a1_intro1"
        );
        println!("{} model entities:", placements.len());
        for p in &placements {
            println!("  {} at {:?} angles {:?}", p.model, p.origin, p.angles);
        }

        world.load_entity_models(&vfs, &mut materials, &device, &placements);
        println!("{}", world.entity_models.summary());
        assert_eq!(world.entity_models.stats.models_missing, 0);
        assert!(world.entity_models.stats.animated > 0, "nothing to animate");

        // Look at the first one from four feet in front and a little above,
        // which is roughly where a player standing next to it would.
        let target_point = placements[0].origin;
        let eye = target_point + Vec3::new(48.0, 48.0, 40.0);
        let camera = Camera::perspective(
            eye,
            glam::Mat4::look_at_rh(eye, target_point, Vec3::Z),
            75.0,
            1.0,
            1.0,
            4096.0,
        );

        let mut context = RenderContext::new(&device, &queue, materials.pipelines());
        let render_target = RenderTarget::new(
            &device,
            "button",
            SIZE,
            SIZE,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        );

        // Only the button: the world around it would swamp the comparison, and
        // what is under test is this module.
        let mut shot = |models: &EntityModels, curtime: f32| -> Vec<u8> {
            context.begin_frame();
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: (SIZE * SIZE * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = context.offscreen_pass(
                    &mut encoder,
                    materials.pipelines(),
                    &render_target,
                    &camera,
                    Load::Clear(wgpu::Color::BLACK),
                );
                models.draw(&mut pass, curtime);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: render_target.color_texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIZE * 4),
                        rows_per_image: Some(SIZE),
                    },
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
                r.expect("readback mapped");
            });
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the queue drained");
            let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
            readback.unmap();
            pixels
        };

        // Up: the sequence the button spawned with, long finished.
        let up = shot(&world.entity_models, 10.0);
        // Against a black clear, so "drawn" is "not still the clear colour".
        // The alpha channel cannot say: `Load::Clear(BLACK)` writes alpha 1.
        let drawn = up.chunks_exact(4).filter(|p| p[0..3] != [0, 0, 0]).count();
        println!("up: {drawn} of {} pixels drawn", SIZE * SIZE);
        assert!(
            drawn > 500,
            "the button drew {drawn} pixels; it is not on screen"
        );

        // Now press it, and look at the two ends of `down`.
        world.entity_models.sync(&[ModelEntity {
            sequence: "down",
            anim_time: 100.0,
            ..placements[0].clone()
        }]);
        let pressing = shot(&world.entity_models, 100.0);
        let pressed = shot(&world.entity_models, 101.0);

        let differences = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
        };
        let moved = differences(&pressing, &pressed);
        println!("down: {moved} pixels differ between cycle 0 and cycle 1");
        assert!(
            moved > 100,
            "the plate drew identically at both ends of `down`: {moved} pixels differ"
        );

        // …and the start of `down` is the end of `up`, because the two are the
        // same travel in opposite directions. This is what says the pose is
        // being applied to the right geometry rather than just to *some*
        // geometry: a wrong bone or a wrong matrix order would not line these
        // two up.
        let same = differences(&up, &pressing);
        println!("up (held) vs down (cycle 0): {same} pixels differ");
        assert!(
            same * 100 < (SIZE * SIZE) as usize,
            "a released button and a just-pressed one should look alike: \
             {same} pixels differ"
        );
    }
}
