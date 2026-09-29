//! The server's half of `point_template` — `TemplateEntities.cpp`, the
//! template pass of `mapentities.cpp`, and `CPointTemplate::CreateInstance`.
//!
//! [`classes::template`] says what the two classes
//! are for. This is the work they cannot do from inside a handler: taking
//! entities out of the map at load and keeping their blocks, and making new
//! entities from those blocks later.

use std::collections::HashMap;

use glam::{Mat3, Mat4, Vec3};

use super::class::TemplateSpawn;
use super::classes::template::{EnvEntityMaker, PointTemplate, TemplateEntry, FIXUP};
use super::entity::{Entity, EntityId};
use super::io::Variant;
use super::movement::{ModelBounds, MoveType};
use super::{attachment, classes, hierarchy, name, Server};
use crate::engine::world::bsp;
use crate::math::{angle_matrix, matrix_angles};

/// `MAX_NUM_TEMPLATES`-worth of fixup numbers: `&0000` to `&9999`, and then
/// round again — *"We won't hit this."*
const INSTANCE_LIMIT: u32 = 10_000;

/// An origin and `QAngle` as one matrix — `SetupMatrixOrgAngles`.
fn placement(origin: Vec3, angles: Vec3) -> Mat4 {
    Mat4::from_translation(origin) * Mat4::from_mat3(angle_matrix(angles))
}

impl Server {
    /// `MapEntity_ParseAllEntites_SpawnTemplates` (`mapentities.cpp:418`),
    /// run inside [`level_init`](Server::level_init) after every block has
    /// become an entity and before any has spawned.
    ///
    /// For each `point_template`, in lump order: every entity named by one of
    /// its sixteen keys has its block recorded, with where it stands relative
    /// to the template, and — unless spawnflag 1 is set, which it is on none
    /// of the game's 302 — is taken out of the map. Then the group's names are
    /// made unique-able ([`reconnect_io`]).
    ///
    /// Returns how many entities left the map.
    pub(super) fn build_templates(&mut self, blocks: &[bsp::Entity], block_of: &[(EntityId, usize)]) -> usize {
        let block_index: HashMap<EntityId, usize> = block_of.iter().copied().collect();
        let templates: Vec<EntityId> = block_of
            .iter()
            .map(|&(id, _)| id)
            .filter(|&id| {
                self.entities
                    .get(id)
                    .is_some_and(|e| e.behaviour.downcast_ref::<PointTemplate>().is_some())
            })
            .collect();
        let mut removed = 0;
        for template in templates {
            let Some(entity) = self.entities.get(template) else {
                continue;
            };
            let Some(class) = entity.behaviour.downcast_ref::<PointTemplate>() else {
                continue;
            };
            let names = class.names.clone();
            let remove = PointTemplate::removes_its_entities(&entity.core);
            let allow_fixup = PointTemplate::allow_name_fixup(&entity.core);
            let world_to_template = placement(entity.core.origin, entity.core.angles).inverse();
            let template_name = entity.core.debug_name().to_owned();

            let mut entries = Vec::new();
            for wanted in &names {
                let found: Vec<EntityId> = name::find_by_name(&self.entities, wanted).collect();
                // The warning is for a name *no block* carries. A name whose
                // class this port has not got is not missing from the map.
                let in_lump = blocks.iter().any(|b| {
                    b.pairs
                        .iter()
                        .any(|(k, v)| k.eq_ignore_ascii_case("targetname") && v.eq_ignore_ascii_case(wanted))
                });
                if found.is_empty() && !in_lump {
                    eprintln!(
                        "source-engine: server: Couldn't find any entities named {wanted}, \
                         which point_template {template_name} is specifying."
                    );
                }
                for id in found {
                    let Some(&index) = block_index.get(&id) else {
                        continue;
                    };
                    let Some(entity) = self.entities.get(id) else {
                        continue;
                    };
                    entries.push(TemplateEntry {
                        block: blocks[index].clone(),
                        entity_to_template: world_to_template
                            * placement(entity.core.origin, entity.core.angles),
                        needs_fixup: false,
                    });
                    if remove {
                        // `UTIL_Remove( pEntity ); gEntList.CleanupDeleteList()`
                        // — gone at once, so that a later template's
                        // `FindEntityByName` cannot find it again.
                        if let Some(at) = self.brush_models.iter().position(|&(_, owner)| owner == id) {
                            let (model, _) = self.brush_models.remove(at);
                            self.templated_brush_models.push(model);
                        }
                        self.entities.mark_for_deletion(id);
                        self.entities.cleanup_delete_list();
                        removed += 1;
                    }
                }
            }
            reconnect_io(&mut entries, allow_fixup);
            if let Some(class) = self
                .entities
                .get_mut(template)
                .and_then(|e| e.behaviour.downcast_mut::<PointTemplate>())
            {
                class.entries = entries;
            }
        }
        self.templated_brush_models.sort_unstable();
        removed
    }

    /// Whether brush model `index` belongs to a templated entity that is not
    /// in the map — so the engine should neither draw it nor collide with it,
    /// though no entity will ever say so.
    pub fn is_templated_brush_model(&self, index: usize) -> bool {
        self.templated_brush_models.binary_search(&index).is_ok()
    }

    /// Every studio model a template can make — `PerformPrecache`, which is
    /// what lets a cube that does not exist at load be drawn and given a
    /// body when it does.
    pub fn precache_models(&self) -> Vec<String> {
        let mut models: Vec<String> = self
            .entities
            .iter()
            .filter_map(|(_, e)| e.behaviour.downcast_ref::<PointTemplate>())
            .flat_map(|template| template.entries.iter())
            .filter_map(|entry| precache_model(&entry.block))
            .filter(|model| !model.starts_with('*') && !model.is_empty())
            .collect();
        models.sort();
        models.dedup();
        models
    }

    /// `CPointTemplate::CreateInstance` and what its two callers do after it
    /// — `InputForceSpawn`'s `OnEntitySpawned`, and `CEnvEntityMaker::SpawnEntity`'s
    /// output and post-spawn velocity.
    pub(super) fn serve_template_spawn(&mut self, request: TemplateSpawn) {
        let spawned = self.spawn_template_instance(request.template, request.origin, request.angles);
        let Some(spawned) = spawned else {
            return;
        };
        match request.maker {
            Some(maker) => {
                // *"Adrian: oops we couldn't spawn the entity (or entities)
                // for some reason!"* — no output for nothing.
                if spawned.is_empty() {
                    return;
                }
                self.dispatch(maker, |core, _, cx| {
                    core.fire_output("OnEntitySpawned", Variant::Void, Some(core.id()), Some(core.id()), 0.0, cx);
                });
                self.apply_post_spawn_velocity(maker, &spawned);
            }
            None => {
                self.dispatch(request.template, |core, _, cx| {
                    core.fire_output("OnEntitySpawned", Variant::Void, Some(core.id()), Some(core.id()), 0.0, cx);
                });
            }
        }
    }

    /// `CreateInstance` itself. `None` is its `return false` — no such
    /// template, or one with nothing in it; `Some` lists what spawned and is
    /// still alive.
    ///
    /// > **A block whose class this port has not got is skipped**, where
    /// > Valve's `MapEntity_ParseEntity` would have made it and a failure
    /// > would have abandoned the whole instance. Templates name 44 classes
    /// > and most are not here; a dropper's cube is.
    pub(super) fn spawn_template_instance(
        &mut self,
        template: EntityId,
        origin: Vec3,
        angles: Vec3,
    ) -> Option<Vec<EntityId>> {
        let entity = self.entities.get(template)?;
        let class = entity.behaviour.downcast_ref::<PointTemplate>()?;
        if class.entries.is_empty() {
            eprintln!(
                "source-engine: server: CreateInstance called on a point_template that has no templates: {}",
                entity.core.debug_name()
            );
            return None;
        }
        let entries = class.entries.clone();
        let allow_fixup = PointTemplate::allow_name_fixup(&entity.core);

        // `Templates_StartUniqueInstance`.
        self.template_instance += 1;
        if self.template_instance >= INSTANCE_LIMIT {
            self.template_instance = 0;
        }
        let suffix = format!("&{:04}", self.template_instance);
        let template_to_world = placement(origin, angles);

        let mut created = Vec::with_capacity(entries.len());
        for entry in &entries {
            let block = match allow_fixup && entry.needs_fixup {
                true => with_instance_names(&entry.block, &suffix),
                false => entry.block.clone(),
            };
            let Some(id) = self.create_from_block(&block) else {
                continue;
            };
            let world = template_to_world * entry.entity_to_template;
            let (origin, angles) = (
                world.w_axis.truncate(),
                matrix_angles(Mat3::from_mat4(world)),
            );
            if let Some(entity) = self.entities.get_mut(id) {
                entity.core.set_abs_placement(origin, angles);
            }
            created.push(id);
        }

        // `SpawnHierarchicalList( iTemplates, pSpawnList, true )`: parents
        // first, then spawn every one, then activate every survivor.
        let ordered = self.spawn_order(&created);
        for &id in &ordered {
            let Some(parent_name) = self.entities.get(id).and_then(|e| e.parent_name.clone()) else {
                continue;
            };
            let parent = name::find_by_name(&self.entities, super::extract_parent_name(&parent_name)).next();
            if let Some(mut entity) = self.entities.detach(id) {
                hierarchy::set_parent(&mut entity.core, &mut self.entities, parent, None, attachment::Poser::NONE);
                self.entities.attach(id, entity);
            }
        }
        for &id in &ordered {
            self.dispatch_spawn(id);
        }
        for &id in &ordered {
            self.dispatch(id, |core, behaviour, cx| {
                if !core.removed {
                    if let Some(name) = core.damage_filter_name.clone() {
                        core.damage_filter = cx.find_by_name(&name);
                    }
                    behaviour.activate(core, cx);
                }
            });
        }
        self.give_bodies(&ordered);
        Some(
            ordered
                .into_iter()
                .filter(|&id| self.entities.get(id).is_some_and(|e| !e.core.removed))
                .collect(),
        )
    }

    /// `MapEntity_ParseEntity` for one block, after the map has loaded:
    /// the class, its keys, its brush model's bounds — and, for a brush
    /// model, the engine's answer to "who places `*N`" from now on.
    fn create_from_block(&mut self, block: &bsp::Entity) -> Option<EntityId> {
        let class = classes::lookup(block.classname()?)?;
        let mut entity = Entity::new(class);
        self.parse_map_data(&mut entity, block);
        let brush_index = entity
            .core
            .model
            .as_deref()
            .and_then(|name| name.strip_prefix('*'))
            .and_then(|n| n.parse::<usize>().ok());
        if let Some(bounds) = brush_index.and_then(|i| self.brush_model_bounds.get(i)) {
            entity.core.model_bounds = ModelBounds {
                mins: bounds.mins,
                maxs: bounds.maxs,
            };
        }
        let id = self.entities.insert(entity);
        if let Some(index) = brush_index.filter(|&i| i != 0) {
            // The last instance of a brush model is the one that places it:
            // the engine has one copy of each.
            match self.brush_models.binary_search_by_key(&index, |&(i, _)| i) {
                Ok(at) => self.brush_models[at].1 = id,
                Err(at) => self.brush_models.insert(at, (index, id)),
            }
        }
        Some(id)
    }

    /// `CreateVPhysics` for what an instance made: the cube's own request is
    /// already queued and is served here, and a brush or studio entity gets
    /// the static or kinematic body it would have got at load.
    fn give_bodies(&mut self, created: &[EntityId]) {
        self.flush_physics();
        let brushes: Vec<(usize, EntityId)> = self
            .brush_models
            .iter()
            .copied()
            .filter(|(_, id)| created.contains(id))
            .collect();
        let Some(physics) = &mut self.physics else {
            return;
        };
        physics.add_brush_entities(&mut self.entities, &brushes);
        physics.add_studio_entities(&mut self.entities);
    }

    /// The post-spawn half of `CEnvEntityMaker::SpawnEntity` — a velocity for
    /// everything that is not `MOVETYPE_NONE`, into the body if it has one.
    fn apply_post_spawn_velocity(&mut self, maker: EntityId, spawned: &[EntityId]) {
        let Some(entity) = self.entities.get(maker) else {
            return;
        };
        let Some(class) = entity.behaviour.downcast_ref::<EnvEntityMaker>() else {
            return;
        };
        // `GetParent() ? GetParent()->GetAbsAngles() : GetAbsAngles()`.
        let angles = entity
            .core
            .parent()
            .and_then(|p| self.entities.get(p))
            .map_or(entity.core.angles, |p| p.core.angles);
        if class.post_spawn_speed == 0.0 {
            return;
        }
        for &id in spawned {
            let moves = self
                .entities
                .get(id)
                .is_some_and(|e| e.core.move_type != MoveType::None);
            if !moves {
                continue;
            }
            let Some(class) = self
                .entities
                .get(maker)
                .and_then(|e| e.behaviour.downcast_ref::<EnvEntityMaker>())
            else {
                return;
            };
            let random = &mut self.random;
            let Some(velocity) = class.shoot_velocity(angles, || random.float(-1.0, 1.0)) else {
                return;
            };
            let body = self.entities.get(id).and_then(|e| e.core.physics);
            match (body, &mut self.physics) {
                (Some(_), Some(physics)) => physics.add_velocity(&self.entities, id, velocity),
                _ => {
                    if let Some(entity) = self.entities.get_mut(id) {
                        entity.core.velocity = velocity;
                    }
                }
            }
        }
    }
}

/// `Templates_ReconnectIOForGroup` (`TemplateEntities.cpp:198`).
///
/// Within one template's group, any value naming another member of the
/// group — the whole value, or the target part of an output connection —
/// gets [`FIXUP`] appended to that name, and so does the member's own
/// `targetname`. Both entries are then marked as needing fixup, and every
/// instance replaces `&0000` with its own number. With names preserved, none
/// of it happens.
///
/// *"FIXME: This is very brittle. Any key with a , will not be found."* —
/// Valve's, and kept: the delimiter is `\x1b` if the value has one and `,`
/// otherwise.
pub(super) fn reconnect_io(entries: &mut [TemplateEntry], allow_fixup: bool) {
    if !allow_fixup {
        return;
    }
    let names: Vec<String> = entries.iter().map(|e| targetname(&e.block).unwrap_or_default()).collect();
    let mut rename = vec![false; entries.len()];
    let mut needs = vec![false; entries.len()];
    for (i, entry) in entries.iter_mut().enumerate() {
        for (key, value) in entry.block.pairs.iter_mut() {
            if key.eq_ignore_ascii_case("targetname") {
                continue;
            }
            let delimiter = match value.contains('\u{1b}') {
                true => '\u{1b}',
                false => ',',
            };
            let (target, rest) = match value.find(delimiter) {
                Some(at) => (value[..at].to_owned(), value[at..].to_owned()),
                None => (value.clone(), String::new()),
            };
            let matches: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|(_, name)| name.eq_ignore_ascii_case(&target))
                .map(|(j, _)| j)
                .collect();
            if matches.is_empty() {
                continue;
            }
            *value = format!("{target}{FIXUP}{rest}");
            needs[i] = true;
            for j in matches {
                rename[j] = true;
                needs[j] = true;
            }
        }
    }
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.needs_fixup = needs[i];
        if rename[i] {
            if let Some((_, value)) = entry
                .block
                .pairs
                .iter_mut()
                .find(|(k, _)| k.eq_ignore_ascii_case("targetname"))
            {
                value.push_str(FIXUP);
            }
        }
    }
}

/// `Templates_GetEntityIOFixedMapData` — every `&` followed by four digits
/// becomes this instance's `&NNNN`.
pub(super) fn with_instance_names(block: &bsp::Entity, suffix: &str) -> bsp::Entity {
    let fix = |value: &str| -> String {
        let bytes = value.as_bytes();
        let mut out = String::with_capacity(value.len());
        let mut i = 0;
        while i < value.len() {
            let is_fixup = bytes[i] == b'&'
                && i + 5 <= bytes.len()
                && bytes[i + 1..i + 5].iter().all(u8::is_ascii_digit);
            if is_fixup {
                out.push_str(suffix);
                i += 5;
                continue;
            }
            let ch = value[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8().max(1);
        }
        out
    };
    bsp::Entity {
        pairs: block.pairs.iter().map(|(k, v)| (k.clone(), fix(v))).collect(),
    }
}

/// What one block's entity would place — a throwaway entity with the block's
/// keys, asked [`Behaviour::precache_model`](super::class::Behaviour::precache_model).
/// Outputs are not parsed; they are not a model.
fn precache_model(block: &bsp::Entity) -> Option<String> {
    let class = classes::lookup(block.classname()?)?;
    let mut entity = Entity::new(class);
    for (key, value) in &block.pairs {
        let Entity { core, behaviour } = &mut entity;
        if !behaviour.key_value(core, key, value) {
            super::keyvalue::base_key_value(core, key, value);
        }
    }
    entity.behaviour.precache_model(&entity.core)
}

fn targetname(block: &bsp::Entity) -> Option<String> {
    block
        .pairs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("targetname"))
        .map(|(_, v)| v.clone())
}

#[cfg(test)]
mod tests;
