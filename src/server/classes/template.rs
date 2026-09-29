//! `point_template` and `env_entity_maker` — how a map makes an entity after
//! it has loaded.
//!
//! ```text
//!   302  point_template     232 preserve names (spawnflag 2), 70 do not; none keeps its entities
//!    94  env_entity_maker   every one spawnflags 0, every one named by ForceSpawn
//! ```
//!
//! `point_template.cpp` (658 lines), `TemplateEntities.cpp` (539) and
//! `env_entity_maker.cpp` (424) are all in the tree and are ported. **A cube
//! dropper is these two classes**: the cube a dropper holds is a template's
//! entity, removed from the map at load, and a relay fires `ForceSpawn` at the
//! maker (58 of the game's 73 cube templates) or at the template itself (15)
//! whenever a cube is wanted — at the start, and again from the cube's own
//! `OnFizzled` when the last one was lost. `sp_a1_intro1`'s one cube is one of
//! them.
//!
//! # Where the work is
//!
//! A template's *data* is built by the server at load, because it is a pass
//! over every entity in the map
//! (`MapEntity_ParseAllEntites_SpawnTemplates`): it records each named
//! entity's keyvalue block and its placement relative to the template, removes
//! the entity, and — unless names are preserved — rewrites the names inside
//! the group so that every instance gets its own (`&0001`, `&0002`, …). That
//! is [`TemplateEntry`] and `Server::build_templates`.
//!
//! *Making* an instance is the server's too, because it is creating and
//! spawning entities, which a handler cannot do from inside the entity list —
//! so `ForceSpawn` asks through [`Context::spawn_template`] and
//! `Server::spawn_template_instance` does it, then fires the output.
//!
//! # Not here
//!
//! - **The VScript hooks** — `PreSpawnInstance` and `PostSpawn`. The only
//!   templates that carry a script are the 45 co-op movie templates
//!   (`coop/mp_coop_movie_level_transition.nut`, each over one
//!   `logic_playmovie`, a class this port has not got).
//! - **`env_entity_maker`'s spawnflags** — autospawn, wait for destruction,
//!   ignore facing, check for space, check player looking. All 94 shipped
//!   makers write 0.

use glam::{Mat4, Vec3};

use crate::engine::world::bsp;
use crate::math::angle_matrix;
use crate::server::class::{Behaviour, Context, InputDef, InputDefs};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};
use crate::server::keyvalue::{atof, atoi, string_to_vector};

/// `SF_POINTTEMPLATE_DONTREMOVETEMPLATEENTITIES` — **no shipped template sets
/// it**, so every templated entity leaves the map at load.
pub const SF_DONT_REMOVE_TEMPLATE_ENTITIES: u32 = 0x0001;

/// `SF_POINTTEMPLATE_PRESERVE_NAMES` — *"Level designers can suppress the
/// uniquification of the spawned entity names with a spawnflag, provided they
/// guarantee that only one instance of the entities will ever be spawned at a
/// time."* All 73 cube templates set it.
pub const SF_PRESERVE_NAMES: u32 = 0x0002;

/// `ENTITYIO_FIXUP_STRING` — appended to a name inside a template group, and
/// replaced with the instance number (`&0001`) each time the group is made.
pub const FIXUP: &str = "&0000";

/// One entity a template makes — `TemplateEntityData_t` and `template_t` in
/// one.
#[derive(Debug, Clone)]
pub struct TemplateEntry {
    /// The entity's keyvalue block from the map, with the group's names
    /// already suffixed with [`FIXUP`] where they need to be unique.
    pub block: bsp::Entity,
    /// `matEntityToTemplate` — where the entity was, in the template's frame.
    pub entity_to_template: Mat4,
    /// `bNeedsEntityIOFixup` — the block contains [`FIXUP`] somewhere.
    pub needs_fixup: bool,
}

/// `CPointTemplate`.
#[derive(Debug, Default)]
pub struct PointTemplate {
    /// `m_iszTemplateEntityNames` — `Template01`…`Template16`.
    pub names: Vec<String>,
    /// `m_hTemplates`, filled by the server at load.
    pub entries: Vec<TemplateEntry>,
}

pub static TEMPLATE_KEYS: &[&str] = &[
    "Template01", "Template02", "Template03", "Template04", "Template05", "Template06",
    "Template07", "Template08", "Template09", "Template10", "Template11", "Template12",
    "Template13", "Template14", "Template15", "Template16",
];

pub static TEMPLATE_INPUTS: InputDefs = &[InputDef::new("ForceSpawn", FieldType::Void)];

pub static TEMPLATE_OUTPUTS: &[&str] = &["OnEntitySpawned"];

impl PointTemplate {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(PointTemplate::default())
    }

    /// `AllowNameFixup`.
    pub fn allow_name_fixup(entity: &EntityCore) -> bool {
        !entity.has_spawn_flags(SF_PRESERVE_NAMES)
    }

    /// `ShouldRemoveTemplateEntities`.
    pub fn removes_its_entities(entity: &EntityCore) -> bool {
        !entity.has_spawn_flags(SF_DONT_REMOVE_TEMPLATE_ENTITIES)
    }
}

impl Behaviour for PointTemplate {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is_template = key.len() == 10
            && key[..8].eq_ignore_ascii_case("Template")
            && key[8..].chars().all(|c| c.is_ascii_digit());
        if !is_template {
            return false;
        }
        if !value.is_empty() {
            self.names.push(value.to_owned());
        }
        true
    }

    /// `InputForceSpawn` — *"Spawn our template"*, at the template's own
    /// placement, then `OnEntitySpawned`. Both are the server's; see the
    /// module docs.
    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("ForceSpawn") {
            cx.spawn_template(entity.id(), entity.origin, entity.angles, None);
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("templates", self.names.join(", ")),
            ("entries", self.entries.len().to_string()),
        ]
    }
}

/// `CEnvEntityMaker` — make a template's entities somewhere else, and send
/// them on their way.
#[derive(Debug, Default)]
pub struct EnvEntityMaker {
    /// `m_iszTemplate` — `EntityTemplate`, a `point_template`'s name.
    pub template: String,
    /// `m_angPostSpawnDirection`.
    pub post_spawn_direction: Vec3,
    /// `m_flPostSpawnDirectionVariance`.
    pub post_spawn_variance: f32,
    /// `m_flPostSpawnSpeed` — **2 of the 94 set it**.
    pub post_spawn_speed: f32,
    /// `m_bPostSpawnUseAngles`.
    pub post_spawn_use_angles: bool,
}

pub static MAKER_KEYS: &[&str] = &[
    "EntityTemplate",
    "PostSpawnDirection",
    "PostSpawnDirectionVariance",
    "PostSpawnSpeed",
    "PostSpawnInheritAngles",
];

pub static MAKER_INPUTS: InputDefs = &[
    InputDef::new("ForceSpawn", FieldType::Void),
    InputDef::new("ForceSpawnAtEntityOrigin", FieldType::String),
];

pub static MAKER_OUTPUTS: &[&str] = &["OnEntitySpawned", "OnEntityFailedSpawn"];

impl EnvEntityMaker {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(EnvEntityMaker::default())
    }

    /// The velocity `SpawnEntity` gives each new entity — `vecShootDir`, with
    /// `random` in place of `random->RandomFloat( -1, 1 )` so that the caller
    /// supplies the stream. `parent_angles` is the maker's parent's angles or
    /// its own, whichever `PostSpawnInheritAngles` reads.
    pub fn shoot_velocity(&self, own_angles: Vec3, mut random: impl FnMut() -> f32) -> Option<Vec3> {
        if self.post_spawn_speed == 0.0 {
            return None;
        }
        let mut direction = self.post_spawn_direction;
        if self.post_spawn_use_angles {
            direction += own_angles;
        }
        let frame = angle_matrix(direction);
        let (forward, left, up) = (frame * Vec3::X, frame * Vec3::Y, frame * Vec3::Z);
        let right = -left;
        let variance = self.post_spawn_variance;
        let mut shoot = forward;
        shoot += right * random() * variance;
        shoot += forward * random() * variance;
        shoot += up * random() * variance;
        Some(shoot.normalize_or_zero() * self.post_spawn_speed)
    }
}

impl Behaviour for EnvEntityMaker {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        match key.to_ascii_lowercase().as_str() {
            "entitytemplate" => self.template = value.to_owned(),
            "postspawndirection" => self.post_spawn_direction = string_to_vector(value),
            "postspawndirectionvariance" => self.post_spawn_variance = atof(value),
            "postspawnspeed" => self.post_spawn_speed = atof(value),
            "postspawninheritangles" => self.post_spawn_use_angles = atoi(value) != 0,
            _ => return false,
        }
        true
    }

    /// `InputForceSpawn` and `InputForceSpawnAtEntityOrigin` — `FindTemplate`
    /// (*"failed to find template"* is Valve's warning), then `SpawnEntity`,
    /// which the server finishes: the instance, `OnEntitySpawned`, the post
    /// spawn velocity.
    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let at_entity = input.name.eq_ignore_ascii_case("ForceSpawnAtEntityOrigin");
        if !at_entity && !input.name.eq_ignore_ascii_case("ForceSpawn") {
            return false;
        }
        let template = cx
            .find_by_name(&self.template)
            .filter(|&id| cx.entity(id).is_some_and(|e| e.behaviour.downcast_ref::<PointTemplate>().is_some()));
        let Some(template) = template else {
            eprintln!(
                "source-engine: server: env_entity_maker {} failed to find template {}.",
                entity.debug_name(),
                self.template
            );
            return true;
        };
        let (mut origin, mut angles) = (entity.origin, entity.angles);
        if at_entity {
            // `SpawnEntity( pTargetEntity->GetAbsOrigin(), GetAbsAngles() )`,
            // and nothing at all if the name does not resolve.
            let name = input.value.to_string();
            let Some(target) = cx.find_by_name(&name).and_then(|id| cx.entity(id)) else {
                return true;
            };
            origin = target.core.origin;
            angles = target.core.angles;
        }
        cx.spawn_template(template, origin, angles, Some(entity.id()));
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("EntityTemplate", self.template.clone()),
            ("PostSpawnSpeed", self.post_spawn_speed.to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_maker_with_no_speed_leaves_its_entities_alone() {
        let maker = EnvEntityMaker::default();
        assert_eq!(maker.shoot_velocity(Vec3::ZERO, || 0.0), None);
    }

    #[test]
    fn a_maker_shoots_along_its_direction_and_inherits_its_angles_when_asked() {
        let mut maker = EnvEntityMaker {
            post_spawn_speed: 100.0,
            ..Default::default()
        };
        let straight = maker.shoot_velocity(Vec3::new(0.0, 90.0, 0.0), || 0.0).unwrap();
        assert!((straight - Vec3::new(100.0, 0.0, 0.0)).length() < 1e-3, "{straight}");
        maker.post_spawn_use_angles = true;
        let turned = maker.shoot_velocity(Vec3::new(0.0, 90.0, 0.0), || 0.0).unwrap();
        assert!((turned - Vec3::new(0.0, 100.0, 0.0)).length() < 1e-3, "{turned}");
    }
}
