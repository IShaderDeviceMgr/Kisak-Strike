//! `weapon_portalgun` — the gun, as an entity.
//!
//! `CWeaponPortalgun` (`game/shared/portal/weapon_portalgun_shared.cpp`,
//! 1,754 lines) on `CBasePortalCombatWeapon` and `CBaseCombatWeapon`. **The
//! server half, `weapon_portalgun.cpp`, is not in this tree** — it is where
//! `give_portalgun` and `upgrade_portalgun` live — so this class is built
//! from the shared half, which has the state, the firing and every rule a shot
//! goes through, and the server half is reconstructed from what calls it
//! ([`portalgun`](crate::server::portalgun) says what was reconstructed and
//! from what).
//!
//! What this struct holds is `CWeaponPortalgun`'s state; what it *does* when
//! the player pulls a trigger is `Server`'s, in
//! [`portalgun`](crate::server::portalgun), because firing is a trace against
//! the whole world and a move of another entity — the portal — which is the
//! kind of work a handler cannot do from inside the entity list.
//!
//! # How a player comes to have one
//!
//! **Three `weapon_portalgun`s are placed in the whole game** — two on
//! `mp_coop_start` and one on `sp_a3_01`, the gun the player wakes up next to
//! after the fall. Everywhere else the gun is *given*, by a command:
//!
//! - `sp_a1_intro3` and `sp_a2_intro`, the two maps where you pick it up off a
//!   pedestal, fire `give weapon_portalgun` at a `point_servercommand` — the
//!   pedestal gun is a `prop_dynamic` that gets `Kill`ed;
//! - every map after that fires `give_portalgun` and, from
//!   `sp_a2_laser_intro` on, `upgrade_portalgun` at `@command`, from
//!   `transitions/sp_transition_list.nut`'s `OnPostTransition`.
//!
//! So the class is mostly the thing those commands make, and the floor
//! pickup is the one `sp_a3_01` uses.
//!
//! # Deliberately absent
//!
//! - **The view model and every effect** — the prongs, the glow sprites, the
//!   beam, the muzzle flash and the sounds. The view model is drawn by the
//!   engine off [`ViewModelState`](crate::server::portalgun::ViewModelState); the rest have no subsystem.
//! - **The falling gun.** `FallInit` gives a dropped weapon a physics body
//!   and lets it settle. The one single-player gun on a floor is
//!   `SF_WEAPON_START_CONSTRAINED`, welded where it was put, so it stays where
//!   the map put it here too.
//! - **`FVisible`** — `BumpWeapon`'s "don't let the player fetch weapons
//!   through walls" trace. The touch box is 36 units round a gun that is on a
//!   floor, and no shipped placement has a wall inside that.
//! - **Linkage groups other than 0**, which are multiplayer's
//!   (`m_iPortalLinkageGroupID = pOwner->entindex()`).

use glam::Vec3;

use crate::server::class::{Behaviour, Context, ModelState, SpawnResult};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::keyvalue::atoi;
use crate::server::movement::{ModelBounds, MoveType, Solid, FSOLID_TRIGGER};

/// The world model — `"playermodel"` in `scripts/weapon_portalgun.txt`.
pub const WORLD_MODEL: &str = "models/weapons/w_portalgun.mdl";
/// The view model — `"viewmodel"` in the same file.
pub const VIEW_MODEL: &str = "models/weapons/v_portalgun.mdl";

/// `w_portalgun.mdl`'s `hull_min`/`hull_max`, which `SetModel` makes the
/// weapon's collision box. Read out of the shipped `.mdl` rather than out of
/// a model loader the server does not have.
const WORLD_MODEL_HULL: (Vec3, Vec3) = (
    Vec3::new(0.0, -5.982_925, -2.795_815),
    Vec3::new(24.658_768, 6.734_901, 8.955_869),
);

/// `CollisionProp()->UseTriggerBounds( true, 36 )` (`CBaseCombatWeapon::Spawn`)
/// — the touch box is the hull bloated 36 units sideways and half that up,
/// and not at all down: *"Don't bloat below, we don't want to trigger it with
/// our heads"*.
const TRIGGER_BLOAT: f32 = 36.0;

/// `SF_WEAPON_NO_PLAYER_PICKUP` (`basecombatweapon_shared.h`).
const SF_WEAPON_NO_PLAYER_PICKUP: u32 = 1 << 1;

/// `portalgun_fire_delay` — how soon after a shot the next may go.
pub const FIRE_DELAY: f32 = 0.20;
/// `portalgun_held_button_fire_fire_delay` — Valve's spelling of the cvar's
/// name — the delay for a button *held* since the last shot.
pub const HELD_BUTTON_FIRE_DELAY: f32 = 0.50;

/// `CWeaponPortalgun`'s state. See the module docs.
#[derive(Debug)]
pub struct WeaponPortalgun {
    /// `m_bCanFirePortal1` — **`true` by default**, from the constructor's
    /// *"TODO: specify these in hammer instead of assuming every gun has blue
    /// chip"*. A gun made by `give weapon_portalgun` is a blue gun.
    pub can_fire_portal1: bool,
    /// `m_bCanFirePortal2` — `false` by default.
    pub can_fire_portal2: bool,
    /// `GetOwner()` — the player carrying it, or `None` on the floor.
    pub owner: Option<EntityId>,
    /// `m_flNextPrimaryAttack` / `m_flNextSecondaryAttack`.
    pub next_primary_attack: f32,
    pub next_secondary_attack: f32,
    /// `m_flNextRepeatPrimaryAttack` / `…Secondary…` — Portal 2's, the later
    /// time a *held* button may fire again.
    pub next_repeat_primary_attack: f32,
    pub next_repeat_secondary_attack: f32,
    /// `m_iLastFiredPortal` — 0, 1 or 2.
    pub last_fired_portal: u8,
    /// `m_iPortalLinkageGroupID` — 0 in single player.
    pub linkage_group: u8,
    /// `m_hPrimaryPortal` / `m_hSecondaryPortal` — found or made on the first
    /// shot of each colour.
    pub primary_portal: Option<EntityId>,
    pub secondary_portal: Option<EntityId>,
    /// `upgrade_potatogun` — the gun has the potato on it: body 1 of the
    /// view model, through [`ViewModelState::body`](crate::server::portalgun::ViewModelState::body).
    pub potato: bool,
}

/// The keys the three placed guns write.
pub static WEAPON_KEYS: &[&str] = &["CanFirePortal1", "CanFirePortal2"];

/// `m_OnPlayerPickup` is `CBaseCombatWeapon`'s — 34 connections on the two
/// maps whose guns are picked up. `OnFiredPortal1`/`2` are the gun's own.
pub static WEAPON_OUTPUTS: &[&str] = &["OnPlayerPickup", "OnFiredPortal1", "OnFiredPortal2"];

impl WeaponPortalgun {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(WeaponPortalgun {
            can_fire_portal1: true,
            can_fire_portal2: false,
            owner: None,
            next_primary_attack: 0.0,
            next_secondary_attack: 0.0,
            next_repeat_primary_attack: 0.0,
            next_repeat_secondary_attack: 0.0,
            last_fired_portal: 0,
            linkage_group: 0,
            primary_portal: None,
            secondary_portal: None,
            potato: false,
        })
    }

    /// `SetCanFirePortal1` / `SetCanFirePortal2`
    /// (`weapon_portalgun_shared.cpp:255` and `:305`), minus the effects —
    /// the prong animation, the view punch, the `powerup` sound and the
    /// `portal_enabled` game event, none of which has a subsystem.
    ///
    /// What is left is the **delay**: an upgrade holds the gun for a quarter
    /// of a second for the blue chip and half a second for the orange, "until
    /// fire animation has completed". Only a gun with an owner gets it —
    /// both functions return before it when there is none.
    pub fn set_can_fire(&mut self, portal2: bool, now: f32) {
        let delay = match portal2 {
            false => {
                self.can_fire_portal1 = true;
                0.25
            }
            true => {
                self.can_fire_portal2 = true;
                0.5
            }
        };
        if self.owner.is_some() {
            self.next_primary_attack = now + delay;
            self.next_secondary_attack = now + delay;
        }
    }

    /// `CBaseCombatWeapon::Equip` and `CWeaponPortalgun::Deploy`, as far as
    /// the gun's own state goes: it has an owner and may fire at once.
    pub fn equip(&mut self, owner: EntityId, now: f32) {
        self.owner = Some(owner);
        self.next_primary_attack = now;
        self.next_secondary_attack = now;
    }

    /// `PostAttack` (`:360`) — the two delays after a shot, and the repeat
    /// delays for a held button.
    pub fn post_attack(&mut self, now: f32) {
        self.next_primary_attack = now + FIRE_DELAY;
        self.next_secondary_attack = now + FIRE_DELAY;
        self.next_repeat_primary_attack = now + HELD_BUTTON_FIRE_DELAY;
        self.next_repeat_secondary_attack = now + HELD_BUTTON_FIRE_DELAY;
    }
}

impl Behaviour for WeaponPortalgun {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("CanFirePortal1") {
            self.can_fire_portal1 = atoi(value) != 0;
            return true;
        }
        if key.eq_ignore_ascii_case("CanFirePortal2") {
            self.can_fire_portal2 = atoi(value) != 0;
            return true;
        }
        false
    }

    /// `CBaseCombatWeapon::Spawn` (`basecombatweapon_shared.cpp:655`) and
    /// `Materialize`: a `SOLID_BBOX` trigger the size of the world model,
    /// bloated for pickup. See the module docs for the fall that is not here.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        entity.model = Some(WORLD_MODEL.to_owned());
        entity.solid = Solid::Bbox;
        entity.solid_flags = FSOLID_TRIGGER;
        entity.move_type = MoveType::None;
        let (mins, maxs) = WORLD_MODEL_HULL;
        entity.model_bounds = ModelBounds {
            mins: mins - Vec3::new(TRIGGER_BLOAT, TRIGGER_BLOAT, 0.0),
            maxs: maxs + Vec3::new(TRIGGER_BLOAT, TRIGGER_BLOAT, TRIGGER_BLOAT * 0.5),
        };
        SpawnResult::Ok
    }

    /// `DefaultTouch` (`basecombatweapon_shared.cpp:1319`) — a player walking
    /// into a gun on the floor. The pickup itself is the *player's*
    /// `BumpWeapon`, so it is asked for here and done by the server; see
    /// [`Context::bump_weapon`].
    fn touch(&mut self, entity: &mut EntityCore, other: EntityId, cx: &mut Context<'_>) {
        if self.owner.is_some() || entity.has_spawn_flags(SF_WEAPON_NO_PLAYER_PICKUP) {
            return;
        }
        if cx.player() != Some(other) {
            return;
        }
        cx.bump_weapon(entity.id());
    }

    /// The world model while it is on the floor, and nothing once it is
    /// carried — `EF_NODRAW` is set by the pickup, and the model state is the
    /// bind pose either way: a gun on a floor does not animate.
    fn model_state(&self) -> Option<ModelState<'_>> {
        Some(ModelState {
            sequence: "",
            cycle: 0.0,
            anim_time: 0.0,
            playback_rate: 0.0,
            skin: 0,
        })
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("CanFirePortal1", self.can_fire_portal1.to_string()),
            ("CanFirePortal2", self.can_fire_portal2.to_string()),
            (
                "owner",
                match self.owner {
                    Some(id) => format!("{}", id.slot()),
                    None => "none".to_owned(),
                },
            ),
            ("last_fired_portal", self.last_fired_portal.to_string()),
        ]
    }
}
