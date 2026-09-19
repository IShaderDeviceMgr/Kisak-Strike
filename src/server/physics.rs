//! The game's half of vphysics — `legacy/game/server/physics.cpp` and the
//! server-side parts of `legacy/game/shared/physics_shared.cpp`.
//!
//! [`crate::vphysics`] is the simulation and knows nothing about entities;
//! this is the entity list's side of it, and the two meet at exactly the line
//! Valve's DLL boundary is: the game owns `physenv`, and the engine hands it
//! collision models. `portdocs/VPHYSICS.md` §5.
//!
//! # The three things `PhysFrame` does, and where each one lands
//!
//! `PhysFrame( TICK_INTERVAL )` (`physics.cpp:1731`) is called from
//! `CPhysicsHook::FrameUpdatePostEntityThink`, so **after every think has run
//! and before the touch pass**. It:
//!
//! 1. steps the environment once;
//! 2. walks `GetActiveObjects` and calls `VPhysicsUpdate` on each owner, which
//!    for `MOVETYPE_VPHYSICS` is `SetAbsOrigin`/`SetAbsAngles` followed by
//!    `PhysicsTouchTriggers`;
//! 3. walks the shadow list and calls `VPhysicsShadowUpdate`.
//!
//! [`Physics::step`] is (1) and (2). (3) is where the *player* would push a
//! cube and is not ported — `portdocs/VPHYSICS.md` §9. What survives of it is
//! the half a door needs, which is the other direction: a mover's pose is
//! pushed *into* the environment before the step, by [`Physics::follow_movers`].
//!
//! # Why a body is not created where Valve creates one
//!
//! `CPhysicsProp::CreateVPhysics` runs inside `Spawn`. It cannot here:
//! `Server::dispatch` lifts the spawning entity out of the entity list for the
//! whole of its own handler, and the environment has to record which entity a
//! body belongs to. So a class **asks** — [`Context::vphysics_init_normal`] —
//! and `Server::flush_physics` builds the body the moment the handler returns,
//! which is the same deferral `Context::create_entity` and
//! `Context::take_damage` already use.
//!
//! [`Context::vphysics_init_normal`]: super::class::Context::vphysics_init_normal

use std::collections::HashMap;

use glam::Vec3;

use super::entity::{EntityId, EntityList};
use super::movement::{MoveType, Solid};
use crate::vphysics::env::{BodyId, Environment, Motion, Sweep};
use crate::vphysics::shadow::PlayerController;
use crate::vphysics::Model;

/// Something a class asked the environment to do, queued until the dispatch it
/// was asked from returns. See the module docs.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// `VPhysicsInitNormal( SOLID_VPHYSICS, 0, asleep )` — a dynamic body from
    /// the entity's own model, at wherever the entity now is.
    InitNormal { asleep: bool },
    /// `IPhysicsObject::EnableMotion`.
    EnableMotion(bool),
    /// `IPhysicsObject::Wake`.
    Wake,
    /// `IPhysicsObject::Sleep`.
    Sleep,
    /// `IPhysicsObject::ApplyForceCenter`, in kg·units/s² — what a
    /// `trigger_push` does to a physics prop.
    ///
    /// `scale_by_mass` is `SF_TRIGGER_PUSH_USE_MASS`
    /// (`triggers_shared.h:30`), and the multiply happens here because the
    /// mass lives here. Valve writes it as
    /// `force *= pPhysObj->GetMass() / DEFAULT_MASS`, which turns the caller's
    /// "100 kg assumed" into the object's real mass. **13 of Portal 2's 192
    /// `trigger_push`es set the flag**, so both branches are live content and
    /// for a 40 kg cube they differ by a factor of 2.5.
    Force { force: Vec3, scale_by_mass: bool },
}

/// One queued [`Request`] and who asked for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub entity: EntityId,
    pub request: Request,
}

/// The environment plus everything needed to tie a body back to an entity.
///
/// `physenv`, `g_PhysWorldObject` and the model cache behind
/// `modelinfo->GetVCollide`, in one place because they have one lifetime: the
/// level's.
pub struct Physics {
    env: Environment,
    /// Studio models by lowercased name — `modelinfo->GetVCollide` for a
    /// `mod_studio`.
    models: HashMap<String, Model>,
    /// The map's brush models by `"*N"` index — the same call for a
    /// `mod_brush`.
    brush_models: HashMap<usize, Model>,
    /// Which entity each live body belongs to. `IPhysicsObject::GetGameData`.
    owners: HashMap<BodyId, EntityId>,
    /// The brush entities whose pose this module pushes into the environment
    /// every tick, because the *game* moves them. `g_pShadowEntities`.
    ///
    /// The third field is the pose last written, so that a door standing still
    /// — which is most of them, most of the time — costs a compare rather than
    /// a write into the broad phase. Valve's equivalent is `PhysFrame`'s
    /// `if ( pPhysics && !pPhysics->IsAsleep() )`.
    movers: Vec<(EntityId, BodyId, (Vec3, Vec3))>,
    /// The player's shadow — `CBasePlayer::m_pPhysicsController`.
    ///
    /// Created on the first tick that has a player rather than in
    /// [`Physics::new`], because the environment is built by the *engine*
    /// before the map's entities are spawned and there is no player then.
    /// `CBasePlayer::InitVCollision` has the same deferral for the same
    /// reason: it runs from `Spawn`, not from `LevelInitPreEntity`.
    player: Option<PlayerController>,
    stats: PhysicsStats,
}

/// What the level's physics did, for `report_entities` and for the depot test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicsStats {
    /// Bodies the map's *static* geometry contributed — the world, its
    /// terrain and its static props. Set by the engine before the server sees
    /// the environment.
    pub static_bodies: usize,
    /// Brush entities given a body: a door, a platform, a clip brush.
    pub brush_bodies: usize,
    /// …of which are kinematic, i.e. movers.
    pub brush_movers: usize,
    /// Entities placing a **studio** model given a body — `prop_dynamic` and
    /// friends. See [`Physics::add_studio_entities`].
    pub studio_bodies: usize,
    /// …of which are kinematic, because they ride a parent.
    pub studio_movers: usize,
    /// …and the ones refused because their model is jointed.
    pub studio_bodies_jointed: usize,
    /// `VPhysicsInitNormal` calls that produced a body.
    pub dynamic_bodies: usize,
    /// …that did not, because the model ships no `.phy`.
    pub dynamic_without_collision: usize,
}

impl Physics {
    /// Takes an environment the engine has already filled with the map's
    /// static geometry, plus the collision models its entities may ask for.
    pub fn new(
        env: Environment,
        models: HashMap<String, Model>,
        brush_models: Vec<(usize, Model)>,
    ) -> Physics {
        let static_bodies = env.len();
        Physics {
            env,
            models,
            brush_models: brush_models.into_iter().collect(),
            owners: HashMap::new(),
            movers: Vec::new(),
            player: None,
            stats: PhysicsStats {
                static_bodies,
                ..Default::default()
            },
        }
    }

    pub fn stats(&self) -> &PhysicsStats {
        &self.stats
    }

    /// `CFuncWall::CreateVPhysics` and friends — every solid brush entity gets
    /// a body, placed from the *entity* rather than from the lump.
    ///
    /// Valve reaches this through a dozen classes' `CreateVPhysics`
    /// overrides, each picking `VPhysicsInitStatic` or `VPhysicsInitShadow`.
    /// This port picks by movetype instead, which lands in the same place: a
    /// `MOVETYPE_PUSH` brush is a shadow object — i.e. kinematic — and
    /// everything else that is solid is static.
    ///
    /// > **Nearly every brush entity is a shadow object, including the ones
    /// > that never move**, and that is deliberate on Valve's part rather than
    /// > an accident of `CFuncBrush::Spawn` setting `MOVETYPE_PUSH` "so it
    /// > doesn't get pushed by anything". `CFuncBrush::CreateVPhysics`
    /// > (`modelentities.cpp:84`) says why: *"Don't init this static. It's
    /// > pretty common for these to be constrained and dynamically parented.
    /// > Initing shadow avoids having to destroy the physics object later and
    /// > lose the constraints."* `CFuncMoveLinear`, `CFuncRotating` and
    /// > `CBaseDoor` all do the same. A kinematic body that is never moved
    /// > behaves exactly as a fixed one does, and
    /// > [`follow_movers`](Physics::follow_movers) skips the ones that have
    /// > not moved.
    ///
    /// The classes that *refuse* a body — triggers, `func_illusionary`, a
    /// `func_brush` with `BRUSHSOLID_NEVER` — are covered by
    /// [`EntityCore::is_solid`](super::entity::EntityCore::is_solid), which
    /// already knows about `FSOLID_NOT_SOLID` and `FSOLID_TRIGGER`. That is
    /// also `CFuncMoveLinear::CreateVPhysics`'s own
    /// `if ( !IsSolidFlagSet( FSOLID_NOT_SOLID ) )`.
    pub fn add_brush_entities(&mut self, entities: &mut EntityList, models: &[(usize, EntityId)]) {
        for &(index, id) in models {
            let Some(model) = self.brush_models.get(&index) else {
                continue;
            };
            let Some(entity) = entities.get_mut(id) else {
                continue;
            };
            if !entity.core.is_solid() {
                continue;
            }
            let motion = brush_motion(&entity.core);
            let hulls = model.hulls.clone();
            let surface = model.params.surface_prop.clone();
            let Some(body) = self.env.add(
                motion,
                &hulls,
                entity.core.origin,
                entity.core.angles,
                &surface,
                None,
            ) else {
                continue;
            };
            entity.core.physics = Some(body);
            self.owners.insert(body, id);
            self.stats.brush_bodies += 1;
            if motion == Motion::Kinematic {
                self.movers
                    .push((id, body, (entity.core.origin, entity.core.angles)));
                self.stats.brush_movers += 1;
            }
        }
    }

    /// `CDynamicProp::CreateVPhysics` (`props.cpp:2119`) — every solid entity
    /// that places a **studio** model gets a body too.
    ///
    /// This is the fourth and last thing that goes into the environment, and
    /// leaving it out is what made a cube fall through the level's furniture:
    /// `prop_dynamic` is 8,072 entities across 105 of the game's 106 maps, and
    /// the panels, hatches and machinery a chamber is built out of are all of
    /// them. `CDynamicProp::CreateVPhysics` ends in `VPhysicsInitStatic()` for
    /// exactly this reason.
    ///
    /// > **A jointed model is skipped.** Valve's route for one is
    /// > `CreateBoneFollowers` — a separate solid entity per collision joint,
    /// > tracking the *animation* — and the prop itself then goes
    /// > `FSOLID_NOT_SOLID`. Bone followers are not ported, so a model whose
    /// > `.phy` carries more than one solid gets nothing rather than a body
    /// > frozen in its bind pose. **51 of the game's 1,056 collision models
    /// > are jointed**, and using solid 0 of one would put a ragdoll's pelvis
    /// > in the way of the player.
    ///
    /// The `SOLID_BBOX`/`SOLID_OBB` branches of `VPhysicsInitStatic` are not
    /// ported either: a `prop_dynamic` reaches them only through
    /// `CDynamicProp::Spawn`'s `solid 0` promotion, which sets
    /// `FSOLID_NOT_SOLID` in the same breath — so
    /// [`EntityCore::is_solid`](super::entity::EntityCore::is_solid) has
    /// already refused them, and 2,622 props the mapper marked non-solid stay
    /// that way.
    pub fn add_studio_entities(&mut self, entities: &mut EntityList) {
        let wanted: Vec<EntityId> = entities
            .iter()
            .filter(|(_, e)| e.core.is_solid())
            .filter(|(_, e)| {
                e.core
                    .model
                    .as_deref()
                    .is_some_and(|m| !m.starts_with('*'))
            })
            .map(|(id, _)| id)
            .collect();
        for id in wanted {
            let Some(entity) = entities.get(id) else {
                continue;
            };
            // A cube has already asked for a *dynamic* body of its own.
            if entity.core.physics.is_some() {
                continue;
            }
            let Some(name) = entity.core.model.clone() else {
                continue;
            };
            let Some(model) = self.models.get(&name.to_ascii_lowercase()) else {
                continue;
            };
            if model.jointed {
                self.stats.studio_bodies_jointed += 1;
                continue;
            }
            let (hulls, surface) = (model.hulls.clone(), model.params.surface_prop.clone());
            let (motion, origin, angles) = {
                let core = &entity.core;
                (studio_motion(core), core.origin, core.angles)
            };
            let Some(body) = self
                .env
                .add(motion, &hulls, origin, angles, &surface, None)
            else {
                continue;
            };
            let Some(entity) = entities.get_mut(id) else {
                continue;
            };
            entity.core.physics = Some(body);
            self.owners.insert(body, id);
            self.stats.studio_bodies += 1;
            if motion == Motion::Kinematic {
                self.movers.push((id, body, (origin, angles)));
                self.stats.studio_movers += 1;
            }
        }
    }

    /// `VPhysicsInitNormal` — a dynamic body from a studio model's `.phy`.
    ///
    /// Returns whether one was made. `false` is `PhysModelCreate` returning
    /// `NULL`, which `CPhysicsProp::CreateVPhysics` answers by dropping to
    /// `SOLID_NONE`/`MOVETYPE_NONE` and warning.
    pub fn init_normal(
        &mut self,
        entities: &mut EntityList,
        id: EntityId,
        asleep: bool,
    ) -> bool {
        let Some(entity) = entities.get_mut(id) else {
            return false;
        };
        // Building a second body for an entity that has one would leak the
        // first. `VPhysicsInitSetup` asserts on exactly this and then destroys
        // it anyway; this does the destroying without the assert.
        if let Some(old) = entity.core.physics.take() {
            self.owners.remove(&old);
            self.movers.retain(|&(_, body, _)| body != old);
            self.env.remove(old);
        }
        let Some(name) = entity.core.model.clone() else {
            return false;
        };
        let Some(model) = self.models.get(&name.to_ascii_lowercase()) else {
            self.stats.dynamic_without_collision += 1;
            return false;
        };
        let hulls = model.hulls.clone();
        let surface = model.params.surface_prop.clone();
        let mass = model.mass;
        let Some(body) = self.env.add(
            Motion::Dynamic,
            &hulls,
            entity.core.origin,
            entity.core.angles,
            &surface,
            Some(mass),
        ) else {
            self.stats.dynamic_without_collision += 1;
            return false;
        };
        entity.core.physics = Some(body);
        // `SetSolid( solidType ); SetMoveType( MOVETYPE_VPHYSICS )`. The
        // solidity is the caller's — `CPhysicsProp` passes `SOLID_VPHYSICS`
        // and the cube's `Spawn` has already set it — but the movetype is
        // `VPhysicsInitNormal`'s own, and it is what the writeback tests.
        entity.core.solid = Solid::VPhysics;
        entity.core.move_type = MoveType::VPhysics;
        self.owners.insert(body, id);
        self.stats.dynamic_bodies += 1;
        if !asleep {
            self.env.wake(body);
        }
        true
    }

    /// `VPhysicsDestroyObject`.
    pub fn destroy(&mut self, entities: &mut EntityList, id: EntityId) {
        let body = match entities.get_mut(id) {
            Some(entity) => entity.core.physics.take(),
            None => None,
        };
        let Some(body) = body else { return };
        self.owners.remove(&body);
        self.movers.retain(|&(_, other, _)| other != body);
        self.env.remove(body);
    }

    /// Applies one queued [`Request`].
    pub fn apply(&mut self, entities: &mut EntityList, pending: &Pending) {
        let body = entities
            .get(pending.entity)
            .and_then(|entity| entity.core.physics);
        match pending.request {
            Request::InitNormal { asleep } => {
                self.init_normal(entities, pending.entity, asleep);
            }
            Request::EnableMotion(enable) => {
                if let Some(body) = body {
                    self.env.enable_motion(body, enable);
                }
            }
            Request::Wake => {
                if let Some(body) = body {
                    self.env.wake(body);
                }
            }
            Request::Sleep => {
                if let Some(body) = body {
                    self.env.sleep(body);
                }
            }
            Request::Force {
                force,
                scale_by_mass,
            } => {
                if let Some(body) = body {
                    let force = match scale_by_mass {
                        true => force * self.env.mass(body).unwrap_or(PUSH_DEFAULT_MASS)
                            / PUSH_DEFAULT_MASS,
                        false => force,
                    };
                    self.env.apply_force_center(body, force);
                }
            }
        }
    }

    /// Where the game says its movers now are — the half of
    /// `VPhysicsShadowUpdate` a door needs.
    ///
    /// Runs **before** the step, because a kinematic body's velocity for this
    /// step is derived from the gap between where it is and where it is told
    /// it will be.
    pub fn follow_movers(&mut self, entities: &EntityList) {
        let env = &mut self.env;
        self.movers.retain_mut(|(id, body, last)| {
            let Some(entity) = entities.get(*id) else {
                return false;
            };
            let now = (entity.core.origin, entity.core.angles);
            if now != *last {
                env.set_kinematic_pose(*body, now.0, now.1);
                *last = now;
            }
            true
        });
    }

    /// One swept box against every physics prop —
    /// [`Environment::sweep_box`], which is the whole of it.
    ///
    /// Here rather than reached through an accessor on the environment because
    /// this is the module that owns the environment's lifetime, and because
    /// the one caller is outside `server/` entirely: `engine/mod.rs`
    /// implements [`PropQuery`](crate::engine::trace::PropQuery) over this and
    /// hands it to the player's movement tracer.
    pub fn sweep_box(&self, half: Vec3, start: Vec3, end: Vec3) -> Option<Sweep> {
        self.env.sweep_box(half, start, end)
    }

    /// `CBasePlayer::SetupVPhysicsShadow` on the first tick there is a player,
    /// then `UpdateVPhysicsPosition` on every tick after it.
    ///
    /// `target` is where the movement put the player, `wish` is
    /// `m_vNewVPhysicsVelocity` and `mins`/`maxs` are the live hull. Returns
    /// whether the shadow is touching something the solver moves —
    /// `IPhysicsPlayerController::IsInContact`.
    ///
    /// **Call it before [`step`](Physics::step)**: it writes the velocity for
    /// the step that is about to run.
    pub fn drive_player(
        &mut self,
        target: Vec3,
        wish: Vec3,
        mins: Vec3,
        maxs: Vec3,
        dt: f32,
    ) -> bool {
        let controller = match &mut self.player {
            Some(controller) => controller,
            None => {
                self.player = PlayerController::new(&mut self.env, target, mins, maxs);
                match &mut self.player {
                    Some(controller) => controller,
                    // A degenerate hull, which cannot happen for a player and
                    // would otherwise retry once a tick forever.
                    None => return false,
                }
            }
        };
        controller.set_bounds(&mut self.env, mins, maxs);
        controller.drive(&mut self.env, target, wish, dt);
        controller.in_contact()
    }

    /// `CBasePlayer::VPhysicsDestroyObject` — the shadow goes when the player
    /// does.
    pub fn destroy_player(&mut self) {
        if let Some(controller) = self.player.take() {
            controller.destroy(&mut self.env);
        }
    }

    /// `physenv->Simulate()` followed by the `GetActiveObjects` loop.
    ///
    /// Returns what moved, as `(entity, origin, angles)` — the caller applies
    /// it, because writing an entity's placement has to go through
    /// [`hierarchy`](super::hierarchy) and this module does not own the list.
    pub fn step(&mut self) -> Vec<(EntityId, Vec3, Vec3)> {
        self.env.step();
        self.env
            .active()
            .filter_map(|(body, origin, angles)| {
                let &id = self.owners.get(&body)?;
                // `IsEntityPositionReasonable` / `IsEntityQAngleReasonable`
                // (`baseentity_shared.cpp:1325`). Valve keeps the last good
                // value and warns; this drops the update, which has the same
                // effect on the entity and does not spam a console that is
                // already the only debugging tool here.
                if !origin.is_finite() || origin.abs().max_element() > MAX_COORD {
                    return None;
                }
                let angles = if angles.is_finite() { angles } else { Vec3::ZERO };
                Some((id, origin, angles))
            })
            .collect()
    }

}

/// Static or kinematic, for an entity that places a **studio** model.
///
/// `CBaseEntity::VPhysicsInitStatic` (`baseentity_shared.cpp`) opens with
///
/// ```text
/// // If this entity has a move parent, it needs to be shadow, not static
/// if ( GetMoveParent() )
///     return VPhysicsInitShadow( false, false );
/// ```
///
/// and that is the *whole* rule — **the movetype is not part of it.** That
/// matters here because `CBaseProp::Spawn` (`props.cpp:253`) sets
/// `MOVETYPE_PUSH` on every prop in the game, exactly as `CFuncBrush::Spawn`
/// does on every brush entity, and for the same reason: "so it doesn't get
/// pushed by anything". Reading the movetype would make all 8,072 of the
/// game's `prop_dynamic`s kinematic bodies whose pose is rewritten every tick,
/// when what they are is furniture that never moves.
///
/// An animating prop is still *static*: `prop_dynamic` animates its **bones**,
/// and its origin does not move. Valve's answer for collision that follows an
/// animation is bone followers, which is the case
/// [`Physics::add_studio_entities`] refuses outright.
fn studio_motion(core: &super::entity::EntityCore) -> Motion {
    match core.parent().is_some() {
        true => Motion::Kinematic,
        false => Motion::Static,
    }
}

/// The same question for a **brush** entity, where the answer is different and
/// is the class's rather than `CBaseEntity`'s.
///
/// Every brush class this port implements reaches `VPhysicsInitShadow`
/// directly — `CFuncBrush` (`modelentities.cpp:90`), `CBaseDoor`
/// (`doors.cpp:415`), `CFuncMoveLinear` (`func_movelinear.cpp:144`) and
/// `CFuncRotating` (`bmodels.cpp:761`) — so a brush entity is kinematic
/// whether it moves or not, and `follow_movers` skips the ones that do not.
/// `CFuncBrush::CreateVPhysics` gives the reason in a comment: *"Don't init
/// this static. It's pretty common for these to be constrained and
/// dynamically parented."*
///
/// The classes that *do* use `VPhysicsInitStatic` for a brush — `CFuncWall`,
/// `CFuncVehicleClip`, `func_lod`, `func_break`, `CEntityBlocker` — are none
/// of them implemented here.
fn brush_motion(_core: &super::entity::EntityCore) -> Motion {
    Motion::Kinematic
}

/// `DEFAULT_MASS` (`triggers.cpp:2581`) — what `CTriggerPush::Touch` assumes
/// an object weighs, and what the caller has already multiplied in.
const PUSH_DEFAULT_MASS: f32 = 100.0;

/// `MAX_COORD_FLOAT` (`public/worldsize.h`) — Valve's
/// `IsEntityPositionReasonable` compares against `MAX_COORD_FLOAT` on each
/// axis.
const MAX_COORD: f32 = 16_384.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::world::bsp;
    use crate::server::name;
    use crate::server::{NoTouchQuery, Server};
    use crate::server::io::Variant;
    use crate::vphysics::collide::{Ledge, Solid as CollideSolid, SolidParams};
    use crate::vphysics::env::{Hulls, Mass};
    use crate::vphysics::Model;
    use std::collections::HashMap;

    fn block(keys: &[(&str, &str)]) -> bsp::Entity {
        bsp::Entity {
            pairs: keys
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    /// A box of the given half-extents about the origin, as one convex piece.
    fn box_solid(half: Vec3) -> CollideSolid {
        let points = (0..8)
            .map(|i| {
                Vec3::new(
                    if i & 1 == 0 { -half.x } else { half.x },
                    if i & 2 == 0 { -half.y } else { half.y },
                    if i & 4 == 0 { -half.z } else { half.z },
                )
            })
            .collect();
        CollideSolid {
            mass_center: Vec3::ZERO,
            rotation_inertia: half * half * (2.0 / 3.0),
            radius: half.length(),
            ledges: vec![Ledge {
                points,
                triangles: Vec::new(),
                material: 0,
                mixed_materials: false,
            }],
        }
    }

    /// A zero-sized brush model, which is what `UTIL_SetModel` gives an
    /// entity whose model the lump does not describe.
    fn empty_model() -> bsp::Model {
        bsp::Model {
            mins: [0.0; 3],
            maxs: [0.0; 3],
            origin: [0.0; 3],
            head_node: 0,
            first_face: 0,
            num_faces: 0,
        }
    }

    fn model(half: Vec3, mass: f32) -> Model {
        let solid = box_solid(half);
        let params = SolidParams {
            mass,
            surface_prop: "metal".to_owned(),
            ..Default::default()
        };
        Model {
            hulls: Hulls::from_solid(&solid),
            mass: Mass::from_solid(&solid, &params),
            jointed: false,
            params,
        }
    }

    /// An environment with a floor at z = 0 and the cube's model in the table.
    fn environment() -> (Environment, HashMap<String, Model>) {
        let mut env = Environment::new(Default::default());
        let floor = box_solid(Vec3::new(1024.0, 1024.0, 32.0));
        env.add(
            Motion::Static,
            &Hulls::from_solid(&floor),
            Vec3::new(0.0, 0.0, -32.0),
            Vec3::ZERO,
            "default",
            None,
        )
        .expect("a floor");
        let mut models = HashMap::new();
        models.insert(
            "models/props/metal_box.mdl".to_owned(),
            model(Vec3::splat(17.8), 40.0),
        );
        (env, models)
    }

    /// The whole seam, end to end: a cube spawns during `level_init`, asks for
    /// a body before there is an environment, gets one when the engine hands
    /// the environment over, and then falls.
    #[test]
    fn a_weighted_cube_falls_once_the_engine_has_handed_over_an_environment() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[
                block(&[("classname", "worldspawn")]),
                block(&[
                    ("classname", "prop_weighted_cube"),
                    ("targetname", "box"),
                    ("origin", "0 0 500"),
                ]),
            ],
            &[],
        );

        let id = name::find_by_name(&server.entities, "box")
            .next()
            .expect("the cube");
        // Before the environment arrives it is exactly what it was before this
        // module existed: solid, and hanging in the air.
        assert_eq!(server.entities.get(id).unwrap().core.origin.z, 500.0);
        assert_eq!(
            server.entities.get(id).unwrap().core.move_type,
            MoveType::None
        );
        assert!(server.entities.get(id).unwrap().core.physics.is_none());

        let (env, models) = environment();
        server.set_physics(env, models, Vec::new());

        let core = &server.entities.get(id).unwrap().core;
        assert!(core.physics.is_some(), "the queued request was served");
        assert_eq!(core.move_type, MoveType::VPhysics);
        assert_eq!(core.solid, Solid::VPhysics);
        assert_eq!(
            server.physics_stats().map(|s| s.dynamic_bodies),
            Some(1)
        );

        // Four seconds of game time at 64 Hz.
        for _ in 0..256 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        let core = &server.entities.get(id).unwrap().core;
        assert!(
            (core.origin.z - 17.8).abs() < 1.0,
            "the cube should be resting on the floor, not at {}",
            core.origin.z
        );
        // The abs/local pair moved together, which is what `set_abs_placement`
        // is for — a bare write to `origin` would leave `local_origin` at 500.
        assert_eq!(core.local_origin, core.origin);
    }

    /// A cube with no collision model is left exactly as it was — Valve's
    /// `PhysModelCreate` returning `NULL`.
    #[test]
    fn a_cube_whose_model_ships_no_phy_is_left_hanging() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[block(&[
                ("classname", "prop_weighted_cube"),
                ("targetname", "box"),
                ("origin", "0 0 500"),
            ])],
            &[],
        );
        let (env, _) = environment();
        server.set_physics(env, HashMap::new(), Vec::new());
        for _ in 0..64 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        let core = &server
            .entities
            .get(name::find_by_name(&server.entities, "box").next().unwrap())
            .unwrap()
            .core;
        assert_eq!(core.origin.z, 500.0);
        assert_eq!(core.move_type, MoveType::None);
        assert_eq!(
            server.physics_stats().map(|s| s.dynamic_without_collision),
            Some(1)
        );
    }

    /// `DisableMotion` freezes it where it is; `EnableMotion` lets it go.
    /// Both were on the unhandled-input list until this module landed.
    #[test]
    fn disable_motion_and_enable_motion_reach_the_solver() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[block(&[
                ("classname", "prop_weighted_cube"),
                ("targetname", "box"),
                ("origin", "0 0 500"),
                // `SF_PHYSPROP_MOTIONDISABLED`, which one shipped cube sets.
                ("spawnflags", "8"),
            ])],
            &[],
        );
        let (env, models) = environment();
        server.set_physics(env, models, Vec::new());
        let id = name::find_by_name(&server.entities, "box").next().unwrap();

        for _ in 0..64 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        assert_eq!(
            server.entities.get(id).unwrap().core.origin.z,
            500.0,
            "frozen by the spawn flag"
        );

        assert!(server.accept_input(id, "EnableMotion", Variant::Void, None, None, 0));
        for _ in 0..256 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        assert!(
            server.entities.get(id).unwrap().core.origin.z < 100.0,
            "and then it falls"
        );
    }

    /// An entity that places a studio model gets a body too — the pass that
    /// stops a cube falling through the level's furniture.
    #[test]
    fn a_solid_prop_dynamic_gets_a_static_body_and_a_parented_one_is_kinematic() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[
                block(&[
                    ("classname", "info_target"),
                    ("targetname", "arm"),
                    ("origin", "0 0 0"),
                ]),
                // `solid 6` is `SOLID_VPHYSICS`, which 5,840 of the game's
                // `prop_dynamic`s write.
                block(&[
                    ("classname", "prop_dynamic"),
                    ("targetname", "panel"),
                    ("model", "models/props/panel.mdl"),
                    ("solid", "6"),
                    ("origin", "0 0 100"),
                ]),
                block(&[
                    ("classname", "prop_dynamic"),
                    ("targetname", "rider"),
                    ("model", "models/props/panel.mdl"),
                    ("solid", "6"),
                    ("parentname", "arm"),
                    ("origin", "0 0 200"),
                ]),
                // `solid 0` — `CDynamicProp::Spawn` promotes it to `SOLID_OBB`
                // *and* `FSOLID_NOT_SOLID`, so it gets nothing.
                block(&[
                    ("classname", "prop_dynamic"),
                    ("targetname", "scenery"),
                    ("model", "models/props/panel.mdl"),
                    ("solid", "0"),
                    ("origin", "0 0 300"),
                ]),
            ],
            &[],
        );
        let (env, mut models) = environment();
        models.insert(
            "models/props/panel.mdl".to_owned(),
            model(Vec3::new(64.0, 64.0, 4.0), 0.0),
        );
        server.set_physics(env, models, Vec::new());
        let stats = server.physics_stats().expect("an environment");
        assert_eq!(stats.studio_bodies, 2, "the two solid panels");
        assert_eq!(stats.studio_movers, 1, "…and the parented one rides");
        let scenery = name::find_by_name(&server.entities, "scenery")
            .next()
            .unwrap();
        assert!(
            server.entities.get(scenery).unwrap().core.physics.is_none(),
            "a `solid 0` prop is not solid and gets no body"
        );
    }

    /// A jointed model gets nothing rather than its first bone frozen in the
    /// bind pose.
    #[test]
    fn a_jointed_model_is_refused_rather_than_frozen_in_its_bind_pose() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[block(&[
                ("classname", "prop_dynamic"),
                ("targetname", "ragdoll"),
                ("model", "models/props/jointed.mdl"),
                ("solid", "6"),
                ("origin", "0 0 100"),
            ])],
            &[],
        );
        let (env, mut models) = environment();
        let mut jointed = model(Vec3::splat(8.0), 0.0);
        jointed.jointed = true;
        models.insert("models/props/jointed.mdl".to_owned(), jointed);
        server.set_physics(env, models, Vec::new());
        let stats = server.physics_stats().expect("an environment");
        assert_eq!(stats.studio_bodies, 0);
        assert_eq!(stats.studio_bodies_jointed, 1);
    }

    /// A cube keeps the **dynamic** body it asked for; the studio pass must
    /// not hand it a static one first.
    #[test]
    fn a_cube_is_not_given_a_static_body_by_the_studio_pass() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[block(&[
                ("classname", "prop_weighted_cube"),
                ("targetname", "box"),
                ("origin", "0 0 500"),
            ])],
            &[],
        );
        let (env, models) = environment();
        server.set_physics(env, models, Vec::new());
        let stats = server.physics_stats().expect("an environment");
        assert_eq!(stats.dynamic_bodies, 1);
        assert_eq!(stats.studio_bodies, 0, "the cube already had one");
        for _ in 0..256 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        let id = name::find_by_name(&server.entities, "box").next().unwrap();
        assert!(
            server.entities.get(id).unwrap().core.origin.z < 100.0,
            "and it is still the dynamic one, so it still falls"
        );
    }

    /// A brush entity that moves becomes a kinematic body and takes its pose
    /// from the entity every tick — the half of `VPhysicsShadowUpdate` a door
    /// needs.
    #[test]
    fn a_solid_brush_entity_gets_a_body_and_a_mover_gets_a_kinematic_one() {
        let mut server = Server::new();
        server.level_init(
            "test",
            &[
                block(&[
                    ("classname", "func_brush"),
                    ("targetname", "wall"),
                    ("model", "*1"),
                    ("origin", "0 0 0"),
                ]),
                block(&[
                    ("classname", "func_movelinear"),
                    ("targetname", "lift"),
                    ("model", "*2"),
                    ("origin", "0 0 0"),
                    ("movedir", "0 0 0"),
                ]),
            ],
            &[empty_model(), empty_model(), empty_model()],
        );
        let (env, models) = environment();
        let brush = model(Vec3::new(64.0, 64.0, 8.0), 0.0);
        server.set_physics(
            env,
            models,
            vec![(1, brush.clone()), (2, brush)],
        );
        let stats = server.physics_stats().expect("an environment");
        assert_eq!(stats.brush_bodies, 2);
        // **Both**, not one: `CFuncBrush::Spawn` sets `MOVETYPE_PUSH` too and
        // `CFuncBrush::CreateVPhysics` deliberately makes it a shadow object
        // rather than a static one. See `add_brush_entities`.
        assert_eq!(stats.brush_movers, 2);
    }
}

/// The whole path on a real map, with no GPU in it.
///
/// `src/vphysics/`'s depot tests drop the cube against the world and its
/// static props; this one adds what only the *server* can add — the map's
/// brush entities and its entity-placed models — and runs the real tick.
#[cfg(test)]
mod depot {
    use super::*;
    use crate::engine::world::bsp::Bsp;
    use crate::engine::world::physics as world_physics;
    use crate::engine::world::props::Props;
    use crate::filesystem::Vfs;
    use crate::server::{name, NoTouchQuery, Server};

    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release drops_its_cube -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn sp_a1_intro1_drops_its_cube_through_the_whole_server_path() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("load the map");
        let props = Props::load("sp_a1_intro1", &bsp).expect("the prop lump");
        let mut built = world_physics::build(
            "sp_a1_intro1",
            &bsp,
            &props,
            &vfs,
            world_physics::surface_properties(&vfs),
        );

        // `Scene::load`'s order, exactly: spawn the entities, read the `.phy`
        // of every model they name, then hand the environment over.
        let mut server = Server::new();
        server.level_init("sp_a1_intro1", &bsp.entities(), &bsp.models);
        let names: Vec<String> = server
            .model_entities()
            .into_iter()
            .map(|e| e.model)
            .collect();
        built.add_models(&names, &vfs);
        server.set_physics(built.environment, built.models, built.brush_models);

        let stats = server.physics_stats().expect("an environment").clone();
        eprintln!("{stats:?}");
        assert_eq!(stats.dynamic_bodies, 1, "the map places one cube");
        assert!(
            stats.brush_bodies > 0,
            "the map's brush entities should have bodies: {stats:?}"
        );
        assert!(
            stats.studio_bodies > 0,
            "…and so should its props: {stats:?}"
        );

        let cube = name::find_by_name(&server.entities, "box")
            .next()
            .expect("the cube named `box`");
        let start = server.entities.get(cube).expect("the cube").core.origin;

        // Five seconds of server time, through `Server::frame` — so the
        // thinks, the touch pass and the physics step all run in the order a
        // running game runs them in.
        for _ in 0..320 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        let core = &server.entities.get(cube).expect("the cube").core;
        eprintln!(
            "cube {start:?} -> {:?}, fell {:.2} units, angles {:?}",
            core.origin,
            start.z - core.origin.z,
            core.angles,
        );
        assert_eq!(core.move_type, MoveType::VPhysics, "it got a real body");
        assert!(
            start.z - core.origin.z > 1.0,
            "the cube should have fallen, moved {:.2}",
            start.z - core.origin.z
        );
        // The abs/local pair moved together — a bare write to `origin` would
        // leave `local_origin` where the lump put it.
        assert_eq!(core.local_origin, core.origin);
    }

    /// The shadow controller, on the real cube on the real map.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release shoves_the_cube -- --ignored --nocapture
    /// ```
    ///
    /// Drops the cube the way the test above does, then puts a player beside
    /// it and walks them into it. What is being tested is the *server* half —
    /// `UpdateVPhysicsPosition` and the controller under it — so the player's
    /// origin is advanced by hand rather than by `client/`'s movement, which
    /// is what the running game's trace would do and which
    /// `a_prop_in_the_clip_chain_stops_a_sweep_the_world_would_not` covers on
    /// its own side.
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn the_player_shadow_shoves_the_cube_on_sp_a1_intro1() {
        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = Vfs::mount_game(&dir, &base, &Default::default()).expect("mount the game");

        let bsp = Bsp::load(&vfs, "sp_a1_intro1").expect("load the map");
        let props = Props::load("sp_a1_intro1", &bsp).expect("the prop lump");
        let mut built = world_physics::build(
            "sp_a1_intro1",
            &bsp,
            &props,
            &vfs,
            world_physics::surface_properties(&vfs),
        );
        let mut server = Server::new();
        server.level_init("sp_a1_intro1", &bsp.entities(), &bsp.models);
        let names: Vec<String> = server
            .model_entities()
            .into_iter()
            .map(|e| e.model)
            .collect();
        built.add_models(&names, &vfs);
        server.set_physics(built.environment, built.models, built.brush_models);

        let cube = name::find_by_name(&server.entities, "box")
            .next()
            .expect("the cube named `box`");
        // Let it land and go to sleep first — the whole point is that a
        // *sleeping* cube is still pushable.
        for _ in 0..320 {
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
        }
        let resting = server.entities.get(cube).expect("the cube").core.origin;

        // The player, standing 48 units west of the cube with their feet at
        // the cube's floor. 48 is the player's own half-width plus the cube's
        // plus a margin, so nothing starts overlapping.
        let feet = Vec3::new(resting.x - 48.0, resting.y, resting.z - 16.0);
        let mut state = crate::server::PlayerState {
            origin: feet,
            angles: Vec3::ZERO,
            velocity: Vec3::new(175.0, 0.0, 0.0),
            base_velocity: Vec3::ZERO,
            on_ground: true,
            move_type: MoveType::Walk,
            mins: Vec3::new(-16.0, -16.0, 0.0),
            maxs: Vec3::new(16.0, 16.0, 72.0),
            health: 100,
            life_state: Default::default(),
            flags: 0,
            buttons: 0,
            // What the movement would have asked for, walking east.
            wish_velocity: Vec3::new(175.0, 0.0, 0.0),
        };
        server.spawn_player(state);

        // A second of walking east, the player's origin advanced the way the
        // movement code would advance it against open floor.
        let mut furthest = 0.0f32;
        for tick in 0..64 {
            state.origin.x += 175.0 / 64.0;
            server.set_player_state(state);
            server.frame(1.0 / 64.0, &mut NoTouchQuery);
            let now = server.entities.get(cube).expect("the cube").core.origin;
            furthest = furthest.max((now - resting).length());
            if tick % 16 == 0 {
                eprintln!("  t{tick}: player {:?} cube {now:?}", state.origin);
            }
        }
        let after = server.entities.get(cube).expect("the cube").core.origin;
        eprintln!(
            "cube rested at {resting:?}, player walked from {feet:?}; \
             shoved {furthest:.1} units at most, ended {after:?}"
        );

        // **The measurement is the furthest it got, not where it ended**, and
        // that is the map rather than the port: the cube on `sp_a1_intro1`
        // comes to rest on the slope it fell onto (`the_cube_on_sp_a1_intro1_
        // falls_and_comes_to_rest`), so a shove eastward is a shove *uphill*
        // and the cube rolls back down behind the player as they walk past it.
        // Watching it go 18 units up the slope is the whole of what the
        // shadow controller does; watching where it settles afterwards is a
        // test of the slope.
        assert!(
            furthest > 8.0,
            "the player should have shoved the cube: it never got further than \
             {furthest:.2} units from {resting:?}"
        );
    }
}
