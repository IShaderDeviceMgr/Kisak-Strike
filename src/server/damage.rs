//! Damage, health and death.
//!
//! `CTakeDamageInfo` (`game/shared/takedamageinfo.{h,cpp}`), the `DMG_*` bit
//! table (`game/shared/shareddefs.h:455`), and the `TakeDamage` →
//! `OnTakeDamage` → `Event_Killed` ladder that every damageable thing in the
//! game goes down.
//!
//! # What this closes
//!
//! Through stage 4 this port had a `trigger_hurt` with complete *timing* and no
//! *effect*: it fired `OnHurt`/`OnHurtPlayer` on exactly the schedule the
//! shipped game fires them and took nothing away, because nothing had health.
//! `portdocs/SERVER.md` stage 5 calls that "the deepest absence in the game
//! layer". This module is it.
//!
//! # The shape, and why it is not Valve's five virtuals
//!
//! Valve's ladder is five overrides deep for a player —
//! `CPortal_Player::OnTakeDamage` → `CBasePlayer::OnTakeDamage` →
//! `CBaseCombatCharacter::OnTakeDamage` → `CPortal_Player::OnTakeDamage_Alive`
//! → `CBaseCombatCharacter::OnTakeDamage_Alive` → `CBaseEntity::OnTakeDamage` —
//! and the `_Alive`/`_Dying`/`_Dead` split exists only so that
//! `CBaseCombatCharacter` can dispatch on `m_lifeState` in one place. Here
//! there is no inheritance, so [`Behaviour::on_take_damage`] is **one** method,
//! [`take_damage`] is the shared arithmetic it calls, and the life-state
//! dispatch is [`take_damage`]'s own `match`.
//!
//! [`Behaviour::on_take_damage`]: super::class::Behaviour::on_take_damage
//!
//! # Damage is deferred by one dispatch
//!
//! `CTriggerHurt::HurtEntity` calls `pOther->TakeDamage( info )` in the middle
//! of its own think — plain re-entrancy, which this module does not have:
//! `Server::dispatch` has lifted the *hurter* out of the entity list for the
//! duration, and applying damage means running the *victim's* virtuals. So
//! [`Context::take_damage`](super::class::Context::take_damage) **queues**,
//! exactly the way [`Context::create_entity`] queues a spawn and
//! [`EntityCore::remove`] queues a deletion, and `Server::dispatch` applies it
//! the moment the current handler returns.
//!
//! [`Context::create_entity`]: super::class::Context::create_entity
//! [`EntityCore::remove`]: super::entity::EntityCore::remove
//!
//! That costs no tick — the flush is inside the same `dispatch` — and it buys
//! the one thing that matters: the gates a caller branches on
//! (`m_takedamage` and `PassesDamageFilter`, plus the trigger's own
//! `PassesTriggerFilters` one level up) are evaluated *before* the queue, so
//! `HurtEntity` still returns the right answer to `HurtAllTouchers` and the
//! half-second think cadence is unchanged.

use super::entity::EntityId;

// ---------------------------------------------------------------------------
// DMG_*
// ---------------------------------------------------------------------------

/// `DMG_GENERIC` — "generic damage was done". Not a bit; the absence of all of
/// them.
pub const DMG_GENERIC: i32 = 0;
/// `DMG_CRUSH` — "crushed by falling or moving object".
///
/// **The commonest damage type in Portal 2**: 71 of the game's 215
/// `trigger_hurt`s name it, which is the crusher plates and the piston fields.
pub const DMG_CRUSH: i32 = 1 << 0;
pub const DMG_BULLET: i32 = 1 << 1;
pub const DMG_SLASH: i32 = 1 << 2;
/// `DMG_BURN` — one `trigger_hurt` and one `filter_damage_type` in the game.
pub const DMG_BURN: i32 = 1 << 3;
pub const DMG_VEHICLE: i32 = 1 << 4;
/// `DMG_FALL` — 34 `trigger_hurt`s.
///
/// > **Nothing in this port ever generates it**, because Portal has no fall
/// > damage — see [`DMG_FALL`]'s note in `rustdocs/SERVER.md`. The 34 are
/// > `trigger_hurt`s at the bottom of pits, which *label* their damage as a
/// > fall and deal it on touch like any other.
pub const DMG_FALL: i32 = 1 << 5;
pub const DMG_BLAST: i32 = 1 << 6;
pub const DMG_CLUB: i32 = 1 << 7;
/// `DMG_SHOCK` — one `trigger_hurt` and one `filter_damage_type`.
pub const DMG_SHOCK: i32 = 1 << 8;
pub const DMG_SONIC: i32 = 1 << 9;
pub const DMG_ENERGYBEAM: i32 = 1 << 10;
/// `DMG_PREVENT_PHYSICS_FORCE` — what `CommitSuicide` sets so that `kill` does
/// not also launch the corpse.
pub const DMG_PREVENT_PHYSICS_FORCE: i32 = 1 << 11;
pub const DMG_NEVERGIB: i32 = 1 << 12;
pub const DMG_ALWAYSGIB: i32 = 1 << 13;
pub const DMG_DROWN: i32 = 1 << 14;
pub const DMG_PARALYZE: i32 = 1 << 15;
/// `DMG_NERVEGAS` — one `trigger_hurt` in the game.
pub const DMG_NERVEGAS: i32 = 1 << 16;
/// `DMG_POISON` — 10 `trigger_hurt`s.
pub const DMG_POISON: i32 = 1 << 17;
/// `DMG_RADIATION` — **the goo**. 27 of the game's `trigger_hurt`s, and the
/// only bit the trigger itself branches on: it selects `RadiationThink`'s
/// quarter-second cadence over `HurtThink`'s half-second one.
pub const DMG_RADIATION: i32 = 1 << 18;
pub const DMG_DROWNRECOVER: i32 = 1 << 19;
/// `DMG_ACID` — 15 `trigger_hurt`s.
pub const DMG_ACID: i32 = 1 << 20;
pub const DMG_SLOWBURN: i32 = 1 << 21;
pub const DMG_REMOVENORAGDOLL: i32 = 1 << 22;
pub const DMG_PHYSGUN: i32 = 1 << 23;
pub const DMG_PLASMA: i32 = 1 << 24;
pub const DMG_AIRBOAT: i32 = 1 << 25;
pub const DMG_DISSOLVE: i32 = 1 << 26;
pub const DMG_BLAST_SURFACE: i32 = 1 << 27;
/// `DMG_DIRECT` — the one bit `FilterDamageType` masks *out* before comparing,
/// so a `filter_damage_type` matching `DMG_BURN` still matches
/// `DMG_BURN|DMG_DIRECT`.
pub const DMG_DIRECT: i32 = 1 << 28;
pub const DMG_BUCKSHOT: i32 = 1 << 29;

// ---------------------------------------------------------------------------
// the state every damageable entity has
// ---------------------------------------------------------------------------

/// `m_takedamage` (`public/const.h:214`) — how this entity answers
/// `TakeDamage`.
///
/// > **Stage 4 had this as a `bool`** and said so: "nothing distinguishes
/// > `DAMAGE_EVENTS_ONLY` from `DAMAGE_YES` without a damage system". There is
/// > one now, and the distinction is one line in [`take_damage`] — but it is
/// > still unreachable from map data, because **no shipped Portal 2 entity is
/// > `DAMAGE_EVENTS_ONLY`**: the mode is set from code (`func_breakable` under
/// > a `spawnflags` bit, `CBaseProp`), and neither class is ported. It is here
/// > because the variant is free and guessing which of the two a future class
/// > wants is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DamageMode {
    /// `DAMAGE_NO`. The default for every entity in the list but the player.
    #[default]
    No,
    /// `DAMAGE_EVENTS_ONLY` — "Call damage functions, but don't modify health".
    EventsOnly,
    /// `DAMAGE_YES`.
    Yes,
}

impl DamageMode {
    /// `m_takedamage != DAMAGE_NO` — the gate `CTriggerHurt::HurtEntity` and
    /// `CBaseEntity::OnTakeDamage` both open with.
    pub fn takes_damage(self) -> bool {
        self != DamageMode::No
    }
}

/// `m_lifeState` (`public/const.h:208`).
///
/// Valve has four values and this has three: `LIFE_RESPAWNABLE` is
/// multiplayer's — where a dead player waits for the round and for a button —
/// and single-player Portal 2 never reaches it, because
/// `sp_fade_and_force_respawn` respawns straight out of
/// [`Dying`](LifeState::Dying) three seconds in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LifeState {
    #[default]
    Alive,
    /// `LIFE_DYING` — killed, the death animation running. Set by
    /// `Event_Killed`.
    Dying,
    /// `LIFE_DEAD` — set by `PlayerDeathThink` on its first run.
    Dead,
}

// ---------------------------------------------------------------------------
// CTakeDamageInfo
// ---------------------------------------------------------------------------

/// `CTakeDamageInfo` (`takedamageinfo.h:24`) — one packet of damage.
///
/// **Four of Valve's sixteen fields**, because the other twelve all need a
/// subsystem this port has not got and a field nothing reads is a field
/// nothing checks. What is gone, and what each would want:
///
/// - `m_vecDamageForce` and `m_vecDamagePosition` — the physics impulse.
///   `GuessDamageForce` computes the force and `VPhysicsTakeDamage` applies
///   it, which needs `rapier`; the *position* exists to aim it, and
///   `CTriggerHurt::HurtEntity` computes it as the nearest point on the
///   victim's collision box to the trigger's centre. `CBaseEntity::OnTakeDamage`
///   has a non-physics impulse as well and it is **unreachable from here
///   anyway**: it demands `!info.GetAttacker()->IsSolidFlagSet( FSOLID_TRIGGER )`,
///   and the only attacker in the port is a `trigger_hurt`.
/// - `m_vecReportedPosition` — the HUD's damage-direction indicator.
/// - `m_hWeapon`, `m_iAmmoType` — a weapon, which is `weapon_portalgun`.
/// - `m_flRadius`, `m_iObjectsPenetrated`, `m_uiBulletID`, `m_uiRecoilIndex`,
///   `m_iDamagedOtherPlayers`, `m_iDamageStats` — explosions, bullets and
///   multiplayer statistics.
/// - `m_iDamageCustom` — a kill type for the death notice.
/// - `m_flMaxDamage` and `m_flBaseDamage` — the latter exists only so that
///   HL2's skill-level adjustment can compute a physics force from the
///   *un*adjusted number, which is a force this port does not apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageInfo {
    /// `m_hInflictor` — "the weapon or rocket (or player) that is dealing the
    /// damage". For a `trigger_hurt`, the trigger.
    pub inflictor: Option<EntityId>,
    /// `m_hAttacker` — "the character who originated the attack". For a
    /// `trigger_hurt`, also the trigger — which is what makes
    /// `CBaseEntity::OnTakeDamage`'s impulse branch unreachable.
    pub attacker: Option<EntityId>,
    /// `m_flDamage`, in points. Already multiplied by the elapsed time for a
    /// `trigger_hurt`, whose `m_flDamage` key is per *second*.
    pub damage: f32,
    /// `m_bitsDamageType` — the `DMG_*` set.
    pub damage_type: i32,
}

impl DamageInfo {
    /// `CTakeDamageInfo( pInflictor, pAttacker, flDamage, bitsDamageType )` —
    /// the four-argument constructor, which is the one every caller in this
    /// port uses.
    pub fn new(
        inflictor: Option<EntityId>,
        attacker: Option<EntityId>,
        damage: f32,
        damage_type: i32,
    ) -> DamageInfo {
        DamageInfo {
            inflictor,
            attacker,
            damage,
            damage_type,
        }
    }

    /// `ScaleDamage`.
    pub fn scale(&mut self, factor: f32) {
        self.damage *= factor;
    }
}

// ---------------------------------------------------------------------------
// the arithmetic
// ---------------------------------------------------------------------------

/// What [`take_damage`] did, so that a caller can tell "refused" from
/// "survived" from "died".
///
/// Valve returns an `int` that is 1 for "took it", 0 for "did not" — and 0 for
/// "died", because `CBaseEntity::OnTakeDamage` returns 0 out of the branch that
/// calls `Event_Killed`. That conflation is why `CBasePlayer::OnTakeDamage`'s
/// "early out if the base class took no damage" also skips everything after a
/// fatal hit, which is deliberate there and a trap to reproduce blindly, so the
/// three cases are named here instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damaged {
    /// `m_takedamage` was `DAMAGE_NO`, or the damage rounded to nothing.
    Refused,
    /// Health came off and the entity is still alive.
    Survived,
    /// Health reached zero. The caller owes `Event_Killed`.
    Killed,
}

/// The health arithmetic shared by every damageable class:
/// `CBaseCombatCharacter::OnTakeDamage_Alive`'s accumulator
/// (`basecombatcharacter.cpp:2538`) over `CBaseEntity::OnTakeDamage`'s
/// subtraction (`baseentity.cpp:1826`).
///
/// Does **not** call `Event_Killed` — that is a virtual on the victim and this
/// is a free function, so it reports [`Damaged::Killed`] and the caller runs
/// it. That split is also what lets the player's version interpose between the
/// two.
///
/// # The fractional accumulator is not decoration
///
/// A `trigger_hurt` with `damage 10` deals `10 * 0.5 = 5` a tick and rounds
/// cleanly; one with `damage 3` would deal `1.5`, and without the accumulator
/// every such hit would truncate to 1 and the trigger would be 33% weaker for
/// ever. Valve keeps the fraction in `m_flDamageAccumulator` and pays it out
/// whenever it reaches a whole point — so the *rate* is right even though each
/// individual hit is an integer.
///
/// > **`flIntegerDamage <= 0` returns without touching health**, which is what
/// > makes a hit of 0.4 free rather than free-and-counted. It is also why
/// > `damage 0` — which 0 of the game's `trigger_hurt`s write, but `ent_fire`
/// > can — is refused rather than fatal.
pub fn take_damage(
    mode: DamageMode,
    health: &mut i32,
    accumulator: &mut f32,
    info: &DamageInfo,
) -> Damaged {
    if !mode.takes_damage() {
        return Damaged::Refused;
    }
    // `if ( m_takedamage != DAMAGE_EVENTS_ONLY )` — the mode that runs the
    // handlers and the outputs and leaves health alone.
    if mode == DamageMode::EventsOnly {
        return Damaged::Survived;
    }

    let fractional = info.damage - info.damage.floor();
    let mut integer = info.damage - fractional;
    *accumulator += fractional;
    if *accumulator >= 1.0 {
        integer += 1.0;
        *accumulator -= 1.0;
    }
    if integer <= 0.0 {
        return Damaged::Refused;
    }

    *health -= integer as i32;
    match *health <= 0 {
        true => Damaged::Killed,
        false => Damaged::Survived,
    }
}

/// `CBaseEntity::TakeHealth` (`baseentity.cpp:1803`) — returns how much was
/// actually restored.
///
/// Refuses anything but `DAMAGE_YES`, and refuses an entity already at its
/// maximum. The only caller is the player's `SetHealth` input taking the
/// upward branch.
pub fn take_health(mode: DamageMode, health: &mut i32, max_health: i32, amount: f32) -> i32 {
    if mode != DamageMode::Yes || *health >= max_health {
        return 0;
    }
    let old = *health;
    *health += amount as i32;
    if *health > max_health {
        *health = max_health;
    }
    *health - old
}

/// `CTakeDamageInfo::DebugGetDamageTypeString` (`takedamageinfo.cpp:392`),
/// for `ent_dump` and the `hurtme`-shaped console output.
///
/// Valve's version writes into a caller's buffer and stops at the first 512
/// characters; this returns the names it recognises, joined, or `"GENERIC"`.
pub fn damage_type_string(bits: i32) -> String {
    if bits == DMG_GENERIC {
        return "GENERIC".to_owned();
    }
    const NAMES: &[(i32, &str)] = &[
        (DMG_CRUSH, "CRUSH"),
        (DMG_BULLET, "BULLET"),
        (DMG_SLASH, "SLASH"),
        (DMG_BURN, "BURN"),
        (DMG_VEHICLE, "VEHICLE"),
        (DMG_FALL, "FALL"),
        (DMG_BLAST, "BLAST"),
        (DMG_CLUB, "CLUB"),
        (DMG_SHOCK, "SHOCK"),
        (DMG_SONIC, "SONIC"),
        (DMG_ENERGYBEAM, "ENERGYBEAM"),
        (DMG_PREVENT_PHYSICS_FORCE, "PREVENT_PHYSICS_FORCE"),
        (DMG_NEVERGIB, "NEVERGIB"),
        (DMG_ALWAYSGIB, "ALWAYSGIB"),
        (DMG_DROWN, "DROWN"),
        (DMG_PARALYZE, "PARALYZE"),
        (DMG_NERVEGAS, "NERVEGAS"),
        (DMG_POISON, "POISON"),
        (DMG_RADIATION, "RADIATION"),
        (DMG_DROWNRECOVER, "DROWNRECOVER"),
        (DMG_ACID, "ACID"),
        (DMG_SLOWBURN, "SLOWBURN"),
        (DMG_REMOVENORAGDOLL, "REMOVENORAGDOLL"),
        (DMG_PHYSGUN, "PHYSGUN"),
        (DMG_PLASMA, "PLASMA"),
        (DMG_AIRBOAT, "AIRBOAT"),
        (DMG_DISSOLVE, "DISSOLVE"),
        (DMG_BLAST_SURFACE, "BLAST_SURFACE"),
        (DMG_DIRECT, "DIRECT"),
        (DMG_BUCKSHOT, "BUCKSHOT"),
    ];
    let named: Vec<&str> = NAMES
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    match named.is_empty() {
        true => format!("{bits:#x}"),
        false => named.join("|"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_point_of_damage_comes_straight_off() {
        let (mut health, mut acc) = (100, 0.0);
        let info = DamageInfo::new(None, None, 25.0, DMG_CRUSH);
        assert_eq!(
            take_damage(DamageMode::Yes, &mut health, &mut acc, &info),
            Damaged::Survived
        );
        assert_eq!(health, 75);
        assert_eq!(acc, 0.0);
    }

    /// `CBaseCombatCharacter::OnTakeDamage_Alive`'s accumulator: four hits of
    /// 2.5 are 10 points, not 8, and the *third* is the one that pays the
    /// fraction back.
    #[test]
    fn fractional_damage_accumulates_rather_than_truncating() {
        let (mut health, mut acc) = (100, 0.0);
        let info = DamageInfo::new(None, None, 2.5, DMG_GENERIC);
        let mut seen = Vec::new();
        for _ in 0..4 {
            take_damage(DamageMode::Yes, &mut health, &mut acc, &info);
            seen.push(health);
        }
        assert_eq!(seen, vec![98, 95, 93, 90]);
        assert_eq!(100 - health, 10);
    }

    /// A hit smaller than a point is refused outright the first time and pays
    /// out on the third, which is the same arithmetic seen from below.
    #[test]
    fn damage_under_one_point_is_refused_until_the_accumulator_fills() {
        let (mut health, mut acc) = (100, 0.0);
        let info = DamageInfo::new(None, None, 0.4, DMG_GENERIC);
        let results: Vec<Damaged> = (0..3)
            .map(|_| take_damage(DamageMode::Yes, &mut health, &mut acc, &info))
            .collect();
        assert_eq!(
            results,
            vec![Damaged::Refused, Damaged::Refused, Damaged::Survived]
        );
        assert_eq!(health, 99);
    }

    #[test]
    fn events_only_runs_the_handlers_and_leaves_health_alone() {
        let (mut health, mut acc) = (100, 0.0);
        let info = DamageInfo::new(None, None, 1000.0, DMG_CRUSH);
        assert_eq!(
            take_damage(DamageMode::EventsOnly, &mut health, &mut acc, &info),
            Damaged::Survived
        );
        assert_eq!(health, 100);
        assert_eq!(
            take_damage(DamageMode::No, &mut health, &mut acc, &info),
            Damaged::Refused
        );
        assert_eq!(health, 100);
    }

    /// A `trigger_hurt` with the smallest damage the shipped game writes —
    /// `damage 10`, which is 5 a half-second tick — takes twenty ticks to kill
    /// a player, and every other one in the game kills on the first.
    #[test]
    fn the_weakest_shipped_trigger_hurt_takes_twenty_doses_to_kill() {
        let (mut health, mut acc) = (100, 0.0);
        let info = DamageInfo::new(None, None, 10.0 * 0.5, DMG_GENERIC);
        let mut doses = 0;
        loop {
            doses += 1;
            if take_damage(DamageMode::Yes, &mut health, &mut acc, &info) == Damaged::Killed {
                break;
            }
            assert!(doses < 100, "never died");
        }
        assert_eq!(doses, 20);
        assert_eq!(health, 0);
    }

    #[test]
    fn health_is_restored_up_to_the_maximum_and_no_further() {
        let mut health = 40;
        assert_eq!(take_health(DamageMode::Yes, &mut health, 100, 30.0), 30);
        assert_eq!(health, 70);
        assert_eq!(take_health(DamageMode::Yes, &mut health, 100, 100.0), 30);
        assert_eq!(health, 100);
        assert_eq!(take_health(DamageMode::Yes, &mut health, 100, 10.0), 0);
        // `TakeHealth` demands `DAMAGE_YES`, where `TakeDamage` only demands
        // "not `DAMAGE_NO`" — Valve's asymmetry, and the reason a
        // `DAMAGE_EVENTS_ONLY` entity can be hurt and cannot be healed.
        let mut health = 40;
        assert_eq!(
            take_health(DamageMode::EventsOnly, &mut health, 100, 30.0),
            0
        );
    }

    #[test]
    fn damage_types_print_by_name() {
        assert_eq!(damage_type_string(DMG_GENERIC), "GENERIC");
        assert_eq!(damage_type_string(DMG_RADIATION), "RADIATION");
        assert_eq!(damage_type_string(DMG_BURN | DMG_DIRECT), "BURN|DIRECT");
    }
}
