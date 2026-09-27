//! The portal gun's server half — how a player gets one, and what a shot
//! does.
//!
//! `weapon_portalgun.cpp` is named by `server_portal_base.vpc` and is **not
//! in this tree**, so two things here are reconstructed rather than ported,
//! and each says so where it is:
//!
//! - **The three commands.** `give_portalgun`, `upgrade_portalgun` and
//!   `upgrade_potatogun` are defined in that file. Their behaviour is pinned
//!   by the one script that relies on all three —
//!   `transitions/sp_transition_list.nut`, which fires `give_portalgun` on
//!   every map from `sp_a1_intro4`, adds `upgrade_portalgun` from
//!   `sp_a2_laser_intro` and swaps it for `upgrade_potatogun` from
//!   `sp_a3_speed_ramp` — and by the `BumpWeapon` hack for `sp_a2_intro`,
//!   whose comment says what `upgrade_portalgun` is for. So: the first gives a
//!   blue gun, the second adds orange, and the third adds orange and the
//!   potato. Each is idempotent, because 60 of the single-player maps have two
//!   `@command`s and every command reaches both.
//! - **`UTIL_FindPlacementHelper`**, the search for an `info_placement_helper`
//!   near a shot. The gun's half of the snap is in the tree and is ported.
//!
//! Everything else is `weapon_portalgun_shared.cpp` and
//! `weapon_portalbasecombatweapon.cpp`, and `portal_placement.cpp` through
//! [`placement`].
//!
//! # A shot, end to end
//!
//! [`Server::player_weapon_frame`] is `ItemPostFrame`: it reads the buttons,
//! honours the fire delays and calls [`Server::fire_portal`], which is
//! `FirePortal` → `TraceFirePortal` → `VerifyPortalPlacement` →
//! `PlacePortal` → `DelayedPlacementThink`. A shot that fails leaves the
//! portal where it was. A shot that succeeds moves it — through
//! [`PropPortal::place_from_gun`](super::classes::PropPortal::place_from_gun),
//! which is `NewLocation` — and so relinks the pair, restarts the opening
//! animation and punches the player out of the wall if the new hole was cut
//! round them, all of which `prop_portal` already did for the map's portals.
//!
//! # What a Portal 2 shot cannot do
//!
//! **Go through a portal.** `TraceFirePortal` traces with
//! `UTIL_Portal_ComplexTraceRay` and then *"Stop segments early if it
//! specifically hit a prop_portal"* — so a shot into a portal ends at its
//! surface, and the portal is on a wall, so the shot is a shot at that wall.
//! This port's traces do not see portals at all, and reach the same wall.

use glam::Vec3;

use super::classes::{self, Player, PropPortal, WeaponPortalgun, IN_ATTACK, IN_ATTACK2};
use super::entity::{Entity, EntityId};
use super::io::Variant;
use super::placement::{self, PlacedBy, PlacementResult, ShotWorld, Trace, MASK_SHOT_PORTAL};
use super::{movement, Server, TouchQuery};
use crate::math::{angle_vectors, vector_angles, vector_angles_forward};

/// `MAX_TRACE_LENGTH` (`public/worldsize.h:32`) — `m_fMaxRange1`.
const MAX_TRACE_LENGTH: f32 = 1.732_050_8 * 2.0 * 16384.0;

/// `SF_NORESPAWN` (`basecombatweapon_shared.h`) — what `GiveNamedItem` adds.
const SF_NORESPAWN: u32 = 1 << 30;

/// The one map `CPortal_Player::BumpWeapon` names (`portal_player.cpp:2614`):
/// *"In Portal 2's incenerator the gun wasn't set correctly and they fired an
/// upgrade_portalgun command to work around it. Now that cheat commands are
/// protected correctly we need a way to give the player both portals without
/// modifying the map."*
const INCINERATOR_MAP: &str = "sp_a2_intro";

/// Where a shot ended up — `TracePortalPlacementInfo_t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot {
    pub result: PlacementResult,
    /// `vecFinalPosition` — where the portal went, or, for a failure, the
    /// point the shot hit.
    pub position: Vec3,
    /// `angFinalAngles`.
    pub angles: Vec3,
    /// The portal that was moved, or would have been.
    pub portal: Option<EntityId>,
}

/// What the renderer needs to draw the gun in the player's hands —
/// `CBaseViewModel`, reduced to a model and a sequence.
///
/// Only the server knows when the gun fired, so the sequence is chosen here;
/// how long it lasts is the model's, so the engine plays it through and then
/// falls back to [`IDLE_SEQUENCE`], which is `WeaponIdle`'s
/// `SendWeaponAnim( ACT_VM_IDLE )` once `HasWeaponIdleTimeElapsed`.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewModelState {
    /// `models/weapons/v_portalgun.mdl`.
    pub model: &'static str,
    /// The sequence last sent — `fire1`, `draw`, or [`IDLE_SEQUENCE`].
    pub sequence: &'static str,
    /// The server's clock when it was sent.
    pub started_at: f32,
    /// `m_nBody` — **1 with the potato on**, 0 without.
    ///
    /// `v_portalgun.mdl` has two body parts: the gun, one model, and
    /// `potatos_vmodel`, whose model 0 is empty and model 1 is PotatOS. The
    /// gun part's single model makes the potato part's `base` 1, so body 1 is
    /// "the potato's model 1". Which body `upgrade_potatogun` sets is
    /// reconstructed from the model, like the skin below.
    pub body: i32,
    /// `m_nSkin` — **the last portal fired**, 0, 1 or 2.
    ///
    /// Reconstructed: `c_weapon_portalgun.cpp`, which sets it, is not in this
    /// tree. What is in the tree is the model, whose three skin families
    /// replace exactly one material — the gun's body — with
    /// `v_portalgun_blue` in family 1 and `v_portalgun_orange` in family 2,
    /// in the order `m_iLastFiredPortal` counts.
    pub skin: i32,
}

/// `ACT_VM_IDLE` on `v_portalgun.mdl`.
pub const IDLE_SEQUENCE: &str = "idle";
/// `ACT_VM_PRIMARYATTACK` — what `FirePortal`'s `SendWeaponAnim` plays for
/// either colour.
const FIRE_SEQUENCE: &str = "fire1";
/// `ACT_VM_DRAW` — `Deploy`.
const DRAW_SEQUENCE: &str = "draw";

impl Server {
    // -----------------------------------------------------------------------
    // Getting one
    // -----------------------------------------------------------------------

    /// The player's portal gun, if they have one —
    /// `Weapon_OwnsThisType( "weapon_portalgun" )`.
    pub fn player_portalgun(&self) -> Option<EntityId> {
        let player = self.entities.get(self.player?)?;
        let weapon = player.behaviour.downcast_ref::<Player>()?.weapon?;
        let gun = self.entities.get(weapon)?;
        gun.behaviour.downcast_ref::<WeaponPortalgun>()?;
        Some(weapon)
    }

    /// The player's gun's state, read-only.
    pub fn portalgun(&self) -> Option<&WeaponPortalgun> {
        let id = self.player_portalgun()?;
        self.entities.get(id)?.behaviour.downcast_ref::<WeaponPortalgun>()
    }

    /// `CBasePlayer::GiveNamedItem` (`player.cpp:5888`) — the `give` command.
    ///
    /// Make the entity at the player's feet, spawn it, and touch it — which
    /// for a weapon is `DefaultTouch` and so a pickup. **Nothing is made if
    /// the player already has one of that type**, which is what makes
    /// `give weapon_portalgun` twice harmless.
    ///
    /// `Err` with the message `give` prints: no player, or a classname this
    /// port has no class for (`"NULL Ent in GiveNamedItem!"`).
    pub fn give_named_item(&mut self, classname: &str) -> Result<Option<EntityId>, String> {
        let Some(player) = self.player.filter(|&id| self.entities.get(id).is_some()) else {
            return Err("no player".to_owned());
        };
        let classname = classname.to_ascii_lowercase();
        if classname == "weapon_portalgun" && self.player_portalgun().is_some() {
            return Ok(None);
        }
        let Some(class) = classes::lookup(&classname) else {
            return Err("NULL Ent in GiveNamedItem!".to_owned());
        };
        let origin = self.entities.get(player).map(|e| e.core.origin).unwrap_or_default();
        let mut entity = Entity::new(class);
        entity.core.set_abs_placement(origin, Vec3::ZERO);
        entity.core.spawn_flags |= SF_NORESPAWN;
        let id = self.entities.insert(entity);
        self.dispatch(id, |core, behaviour, cx| {
            behaviour.spawn(core, cx);
        });
        if self.entities.get(id).is_some_and(|e| !e.core.removed) {
            // `pent->Touch( this )`.
            self.dispatch(id, |core, behaviour, cx| behaviour.touch(core, player, cx));
        }
        Ok(Some(id))
    }

    /// `give_portalgun` — reconstructed; see the module docs. A blue gun, or
    /// the blue chip on the gun the player already has.
    pub fn give_portalgun(&mut self) -> Result<(), String> {
        self.give_named_item("weapon_portalgun")?;
        self.upgrade_portalgun_chips(false, false)
    }

    /// `upgrade_portalgun` — reconstructed. Both chips, on the gun the
    /// player has; nothing without one, as `Weapon_OwnsThisType` returning
    /// null does nothing in every command of this shape.
    pub fn upgrade_portalgun(&mut self) -> Result<(), String> {
        self.upgrade_portalgun_chips(true, false)
    }

    /// `upgrade_potatogun` — reconstructed. `upgrade_portalgun` and the
    /// potato: the transition script swaps it in *for* `upgrade_portalgun`
    /// from `sp_a3_speed_ramp` on, so it has to leave a dual gun.
    pub fn upgrade_potatogun(&mut self) -> Result<(), String> {
        self.upgrade_portalgun_chips(true, true)
    }

    fn upgrade_portalgun_chips(&mut self, orange: bool, potato: bool) -> Result<(), String> {
        let Some(gun) = self.player_portalgun() else {
            return Err("the player has no portal gun".to_owned());
        };
        let now = self.clock.time().curtime;
        let Some(class) = self
            .entities
            .get_mut(gun)
            .and_then(|e| e.behaviour.downcast_mut::<WeaponPortalgun>())
        else {
            return Ok(());
        };
        class.set_can_fire(false, now);
        if orange {
            class.set_can_fire(true, now);
        }
        class.potato |= potato;
        Ok(())
    }

    /// `CPortal_Player::BumpWeapon` (`portal_player.cpp:2555`) and the
    /// `OnPickedUp` after it — the player has walked into, or been handed,
    /// `weapon`.
    ///
    /// **A second portal gun is not a second weapon**: its chips are copied
    /// onto the one the player has and it is removed. Otherwise the gun is
    /// equipped — owned, no longer a trigger, not drawn in the world — and
    /// deployed, and anything the player was carrying is dropped, *"If we're
    /// holding and object before picking up portalgun, drop it"*.
    pub(super) fn bump_weapon(&mut self, weapon: EntityId) {
        let Some(player) = self.player else { return };
        let Some((owned, can1, can2)) = self
            .entities
            .get(weapon)
            .and_then(|e| e.behaviour.downcast_ref::<WeaponPortalgun>())
            .map(|gun| (gun.owner.is_some(), gun.can_fire_portal1, gun.can_fire_portal2))
        else {
            return;
        };
        // `if ( pOwner || !Weapon_CanUse( pWeapon ) || … ) return false;`
        if owned {
            return;
        }
        let alive = self.entities.get(player).is_some_and(|e| e.core.is_alive());
        if !alive {
            return;
        }
        let now = self.clock.time().curtime;

        if let Some(existing) = self.player_portalgun() {
            if let Some(gun) = self
                .entities
                .get_mut(existing)
                .and_then(|e| e.behaviour.downcast_mut::<WeaponPortalgun>())
            {
                if can1 {
                    gun.set_can_fire(false, now);
                }
                if can2 {
                    gun.set_can_fire(true, now);
                }
            }
            self.dispatch(weapon, |core, _, cx| {
                core.fire_output("OnPlayerPickup", Variant::Void, Some(player), Some(core.id()), 0.0, cx);
                core.remove();
            });
            return;
        }

        let incinerator = self.map.as_deref() == Some(INCINERATOR_MAP);
        if let Some(entity) = self.entities.get_mut(weapon) {
            // `CBaseCombatWeapon::Equip`: `RemoveSolidFlags( FSOLID_TRIGGER )`,
            // `FollowEntity( pOwner )` — and in first person the followed world
            // model is not drawn.
            entity.core.solid_flags &= !movement::FSOLID_TRIGGER;
            entity.core.effects |= movement::EF_NODRAW;
            if let Some(gun) = entity.behaviour.downcast_mut::<WeaponPortalgun>() {
                if incinerator {
                    gun.can_fire_portal1 = true;
                    gun.can_fire_portal2 = true;
                }
                gun.equip(player, now);
            }
        }
        if let Some(class) = self
            .entities
            .get_mut(player)
            .and_then(|e| e.behaviour.downcast_mut::<Player>())
        {
            class.weapon = Some(weapon);
        }
        // `Deploy` → `DefaultDeploy` → `SendWeaponAnim( ACT_VM_DRAW )`.
        self.view_model_sequence = (DRAW_SEQUENCE, now);
        // `ForceDropOfCarriedPhysObjects`.
        self.drop_carried(true);
        self.dispatch(weapon, |core, _, cx| {
            core.fire_output("OnPlayerPickup", Variant::Void, Some(player), Some(core.id()), 0.0, cx);
        });
    }

    /// The gun in the player's hands, for the renderer; `None` without one.
    pub fn view_model(&self) -> Option<ViewModelState> {
        let gun = self.portalgun()?;
        let alive = self
            .player
            .and_then(|id| self.entities.get(id))
            .is_some_and(|e| e.core.is_alive());
        if !alive {
            return None;
        }
        let (sequence, started_at) = self.view_model_sequence;
        Some(ViewModelState {
            model: classes::weapon::VIEW_MODEL,
            sequence,
            started_at,
            body: i32::from(gun.potato),
            skin: i32::from(gun.last_fired_portal),
        })
    }

    // -----------------------------------------------------------------------
    // Firing
    // -----------------------------------------------------------------------

    /// `CBasePlayer::ItemPostFrame` into `CBasePortalCombatWeapon::ItemPostFrame`
    /// (`weapon_portalbasecombatweapon.cpp:436`) — once a tick, for the gun
    /// the player is holding.
    ///
    /// **No shot while carrying something**: `ItemPostFrame` returns straight
    /// after `if ( m_hUseEntity != NULL )`, and the pickup controller is the
    /// use entity for as long as a cube is held.
    ///
    /// A held button fires again only once the longer repeat delay is up —
    /// `m_afButtonLast` is last tick's buttons, so "held" means "down now and
    /// down then". And the primary branch **returns** whichever way it goes,
    /// so holding both buttons fires only blue.
    pub(super) fn player_weapon_frame(&mut self, query: &mut dyn TouchQuery) {
        let Some(player) = self.player else { return };
        let Some(entity) = self.entities.get(player) else { return };
        if !entity.core.is_alive() || self.carry.is_some() {
            return;
        }
        let Some(class) = entity.behaviour.downcast_ref::<Player>() else {
            return;
        };
        let (buttons, last) = (class.buttons(), class.last_buttons());
        let Some(gun_id) = self.player_portalgun() else { return };
        let Some(gun) = self
            .entities
            .get(gun_id)
            .and_then(|e| e.behaviour.downcast_ref::<WeaponPortalgun>())
        else {
            return;
        };
        let now = self.clock.time().curtime;

        if buttons & IN_ATTACK != 0 && gun.next_primary_attack <= now {
            if last & IN_ATTACK != 0 && gun.next_repeat_primary_attack > now {
                return;
            }
            self.attack(gun_id, false, query);
            return;
        }
        if buttons & IN_ATTACK2 != 0 && gun.next_secondary_attack <= now {
            if last & IN_ATTACK2 != 0 && gun.next_repeat_secondary_attack > now {
                return;
            }
            self.attack(gun_id, true, query);
        }
    }

    /// `PrimaryAttack`/`SecondaryAttack` (`:430`, `:463`) — refused without the
    /// chip, and otherwise `FirePortal1`/`2`, then `PostAttack`.
    ///
    /// **`OnFiredPortal1` fires from the primary attack and nothing fires from
    /// the secondary**: `SecondaryAttack` has no `m_OnFiredPortal2.FireOutput`,
    /// though the output is declared.
    fn attack(&mut self, gun: EntityId, portal2: bool, query: &mut dyn TouchQuery) {
        let Some(player) = self.player else { return };
        let can = self
            .entities
            .get(gun)
            .and_then(|e| e.behaviour.downcast_ref::<WeaponPortalgun>())
            .is_some_and(|g| match portal2 {
                false => g.can_fire_portal1,
                true => g.can_fire_portal2,
            });
        if !can {
            return;
        }
        self.fire_portal(gun, portal2, query);
        if !portal2 {
            self.dispatch(gun, |core, _, cx| {
                core.fire_output("OnFiredPortal1", Variant::Void, Some(player), Some(core.id()), 0.0, cx);
            });
        }
        let now = self.clock.time().curtime;
        if let Some(g) = self
            .entities
            .get_mut(gun)
            .and_then(|e| e.behaviour.downcast_mut::<WeaponPortalgun>())
        {
            g.post_attack(now);
        }
    }

    /// `CWeaponPortalgun::FirePortal1`/`2` and `FirePortal` (`:670`, `:772`,
    /// `:874`): find the portal, trace, place, and on success move it.
    ///
    /// Also the `fire_portal` path for a test, which is why it returns what
    /// happened.
    pub fn fire_portal(&mut self, gun: EntityId, portal2: bool, query: &mut dyn TouchQuery) -> Option<Shot> {
        let player = self.player?;
        let (group, held) = {
            let g = self.entities.get(gun)?.behaviour.downcast_ref::<WeaponPortalgun>()?;
            let held = match portal2 {
                false => g.primary_portal,
                true => g.secondary_portal,
            };
            (g.linkage_group, held)
        };

        // `if( m_hPrimaryPortal.Get() == NULL ) m_hPrimaryPortal =
        // CProp_Portal::FindPortal( m_iPortalLinkageGroupID, false, true );`
        let portal = match held.filter(|&id| self.entities.get(id).is_some()) {
            Some(id) => id,
            None => {
                let id = self.find_portal(group, portal2, true)?;
                if let Some(g) = self
                    .entities
                    .get_mut(gun)
                    .and_then(|e| e.behaviour.downcast_mut::<WeaponPortalgun>())
                {
                    match portal2 {
                        false => g.primary_portal = Some(id),
                        true => g.secondary_portal = Some(id),
                    }
                }
                id
            }
        };

        // `pPortal->SetFiredByPlayer( pPlayer )` — before the trace, which
        // reads it through the bump rules.
        let (half_width, half_height) = {
            let p = self
                .entities
                .get_mut(portal)?
                .behaviour
                .downcast_mut::<PropPortal>()?;
            p.fired_by = Some(player);
            (p.half_width, p.half_height)
        };

        let (origin, view) = {
            let e = self.entities.get(player)?;
            (e.core.origin, e.core.angles)
        };
        let eye = origin + self.player_view_offset;
        let (direction, _, _) = angle_vectors(view);
        let now = self.clock.time().curtime;
        self.view_model_sequence = (FIRE_SEQUENCE, now);

        let mut shot = {
            let mut world = ShotWorld::new(
                query,
                self.physics.as_ref(),
                &self.entities,
                &self.brush_models,
                Some(player),
            );
            trace_fire_portal(
                &mut world,
                portal,
                eye,
                direction,
                half_width,
                half_height,
                PlacedBy::Player,
            )
        };

        // "the up vector tends to have little bit of wiggle in it in the
        // millionths decimal place. Stomp out even the smallest disagreement
        // here." — `floor( x * 512 ) / 512` on every component.
        shot.position = (shot.position * 512.0).floor() / 512.0;

        // `PlacePortal` returns early on a failure, and `DelayedPlacementThink`
        // re-checks the two things that could have changed mid-flight — which
        // here is nothing, but the re-check is at the *rounded* position, and
        // that is not nothing.
        if shot.result.succeeded() {
            let world = ShotWorld::new(
                query,
                self.physics.as_ref(),
                &self.entities,
                &self.brush_models,
                Some(player),
            );
            let (forward, _, _) = angle_vectors(shot.angles);
            let (overlap, _) = placement::overlapping_other_portals(
                &world,
                portal,
                shot.position,
                shot.angles,
                half_width,
                half_height,
                false,
            );
            if overlap != PlacementResult::Success {
                shot.result = PlacementResult::OverlapLinked;
            } else if placement::is_intersecting_no_portal_volume(
                &world,
                shot.position,
                shot.angles,
                forward,
                half_width,
                half_height,
            ) {
                shot.result = PlacementResult::InvalidVolume;
            }
        }

        if shot.result.succeeded() {
            let (position, angles) = (shot.position, shot.angles);
            self.dispatch(portal, |core, behaviour, cx| {
                if let Some(p) = behaviour.downcast_mut::<PropPortal>() {
                    p.place_from_gun(core, position, angles, gun, true, cx);
                }
            });
            if let Some(g) = self
                .entities
                .get_mut(gun)
                .and_then(|e| e.behaviour.downcast_mut::<WeaponPortalgun>())
            {
                g.last_fired_portal = if portal2 { 2 } else { 1 };
            }
        }
        Some(shot)
    }
}

/// `CWeaponPortalgun::TraceFirePortal` (`weapon_portalgun_shared.cpp:1213`)
/// — where a shot from `start` along `direction` puts `portal`.
#[allow(clippy::too_many_arguments)]
pub fn trace_fire_portal(
    world: &mut ShotWorld<'_>,
    portal: EntityId,
    start: Vec3,
    direction: Vec3,
    half_width: f32,
    half_height: f32,
    placed_by: PlacedBy,
) -> Shot {
    let end = start + direction * MAX_TRACE_LENGTH;
    let trace = world.trace_line(start, end, MASK_SHOT_PORTAL);

    // "Test for hitting a pass-thru surface" — which is also "hit nothing".
    if !trace.hit.did_hit() || trace.hit.start_solid {
        return Shot {
            result: PlacementResult::PassthroughSurface,
            position: trace.hit.end,
            angles: vector_angles_forward(-direction),
            portal: Some(portal),
        };
    }

    // "Clip this to any number of entities that can block us"
    if let Some(shot) = clipped_by_blockers(world, &trace, direction, portal) {
        return shot;
    }

    // "Create a pseudo "up" vector from the portal if it's on the floor or
    // ceiling" — and the player is always upright here: there is no paint to
    // stick them to a wall.
    let normal = trace.hit.normal;
    let up = match normal.x.abs() < 0.001 && normal.y.abs() < 0.001 {
        true => direction,
        false => Vec3::Z,
    };
    let angles = vector_angles(normal, up);
    let position = trace.hit.end;

    if let Some(shot) = snap_to_helper(world, &trace, portal, angles, half_width, half_height, placed_by) {
        return shot;
    }

    let mut position = position;
    let (result, _) = placement::verify_and_fizzle_blocking(
        world,
        portal,
        &mut position,
        angles,
        half_width,
        half_height,
        placed_by,
    );
    Shot {
        result,
        // "If it was a failure, put the effect at exactly where the player
        // shot instead of where the portal bumped to"
        position: match result.succeeded() {
            true => position,
            false => trace.hit.end,
        },
        angles,
        portal: Some(portal),
    }
}

/// `PortalTraceClippedByBlockers` (`:1383`), for the blocker single player
/// has: an **enabled fizzler** between the gun and the wall stops the shot
/// at its own box, facing back along the shot.
///
/// The box is `WorldSpaceSurroundingBounds`, an AABB, not the fizzler's
/// brushes — so a fizzler turned at an angle stops a shot a little before its
/// field. The rotating-door half is for a class no Portal 2 map places.
fn clipped_by_blockers(
    world: &ShotWorld<'_>,
    trace: &Trace,
    direction: Vec3,
    portal: EntityId,
) -> Option<Shot> {
    let (start, end) = (trace.hit.start, trace.hit.end);
    let nearest = world
        .bumpers
        .iter()
        .filter(|b| b.kind == placement::BumperKind::Cleanser)
        .filter_map(|b| placement::intersect_ray_with_box(start, end, b.mins, b.maxs))
        .min_by(|a, b| {
            let da = (a.end - a.start).length_squared();
            let db = (b.end - b.start).length_squared();
            da.total_cmp(&db)
        })?;
    Some(Shot {
        result: PlacementResult::Cleanser,
        position: nearest.end,
        angles: vector_angles_forward(-direction),
        portal: Some(portal),
    })
}

/// `AttemptSnapToPlacementHelper` (`:1625`) — a shot that lands near an
/// `info_placement_helper` is re-aimed at the helper's own spot on the same
/// surface, and, if the helper says so, at its angles.
///
/// The helper is **refused** if the re-traced surface does not face the way
/// the shot's did (to within `FLT_EPSILON` a component), if the shot landed
/// outside its radius of that spot, or if the portal will not fit there —
/// and then the shot places normally.
fn snap_to_helper(
    world: &mut ShotWorld<'_>,
    trace: &Trace,
    portal: EntityId,
    angles: Vec3,
    half_width: f32,
    half_height: f32,
    placed_by: PlacedBy,
) -> Option<Shot> {
    let hit = trace.hit.end;
    let helper = find_placement_helper(world, hit)?;

    let normal = trace.hit.normal;
    let start = normal + helper.origin;
    let direction = (-normal).normalize_or_zero();
    let on_helper = world.trace_line(start, start + direction * MAX_TRACE_LENGTH, MASK_SHOT_PORTAL);
    let helper_angles = match helper.use_angles {
        true => helper.angles,
        false => angles,
    };

    let same_face = (on_helper.hit.normal - normal)
        .abs()
        .max_element()
        <= f32::EPSILON;
    let within = (hit - on_helper.hit.end).length_squared() <= helper.radius * helper.radius;
    if !same_face || !within {
        return None;
    }
    let mut position = on_helper.hit.end;
    let (result, _) = placement::verify_and_fizzle_blocking(
        world,
        portal,
        &mut position,
        helper_angles,
        half_width,
        half_height,
        placed_by,
    );
    if !result.succeeded() {
        return None;
    }
    Some(Shot {
        result: PlacementResult::UsedHelper,
        position,
        angles: helper_angles,
        portal: Some(portal),
    })
}

/// `UTIL_FindPlacementHelper` — **reconstructed**; its source is not in this
/// tree. The nearest enabled helper whose radius reaches `position`.
fn find_placement_helper(world: &ShotWorld<'_>, position: Vec3) -> Option<placement::Helper> {
    world
        .helpers
        .iter()
        .filter(|h| h.origin.distance_squared(position) <= h.radius * h.radius)
        .min_by(|a, b| {
            a.origin
                .distance_squared(position)
                .total_cmp(&b.origin.distance_squared(position))
        })
        .copied()
}

#[cfg(test)]
mod tests;
