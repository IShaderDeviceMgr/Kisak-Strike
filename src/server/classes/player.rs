//! The player, as an entity: health, death, and the move type.
//!
//! `CBasePlayer` (`game/server/player.cpp`, 9,940 lines) and
//! `CPortal_Player` (`game/server/portal/portal_player.cpp`, 5,737), reduced to
//! the part that decides what happens when something hurts you.
//!
//! # Two stages built this, and the split is worth knowing
//!
//! **Stage 4** put a box with `FL_CLIENT` in the entity list, because a touch
//! is a fact about *two* entities: `PassesTriggerFilters` tests `FL_CLIENT` on
//! the toucher, `CTriggerHurt` picks its output by `IsPlayer()`,
//! `CFilterName` special-cases the literal string `!player`, and **121 of the
//! game's 128 `point_teleport`s target `!player`**. That box held no state at
//! all: everything about it arrived once a tick as
//! [`PlayerState`](crate::server::PlayerState).
//!
//! **Stage 5** is what was left — `CBasePlayer` proper — and it is the point at
//! which the box stops being stateless. Health, life state, the move type and
//! the button mask are the server's now, and the client reads them back. See
//! [`PlayerState`](crate::server::PlayerState)'s docs for which fields changed
//! direction and why.
//!
//! # The movement did **not** move here, and that is a decision
//!
//! `portdocs/SERVER.md` stage 5 lists "the movement moving to the server (and
//! with it the question of what to do about two clocks)". The question is
//! answered in §5 of that document and the answer is no: `CPlayerMove` runs on
//! the fixed tick and `CPrediction` re-runs the *same* movement code on the
//! client, so a port with one process and no `net/` already has the client
//! half and gains nothing but a 64 Hz camera by moving it. What stage 5 moves
//! is the *authority*: the move type, the health and the life state are server
//! state, `client/` reads them, and `FullWalkMove` still runs on the rendered
//! frame.
//!
//! # What is still not here
//!
//! The weapon (Portal 2's is `weapon_portalgun`, 3 placed, and it needs the
//! portal system), the armour (`m_ArmorValue`; Portal has no armour and no
//! pickup that gives any), drowning, the suit, the HUD, teams, observer mode,
//! and `CBasePlayer::PreThink`/`PostThink` beyond the two lines
//! `logic_playerproxy` needs. Each is a measurement in `rustdocs/SERVER.md`
//! rather than a stub here.

use crate::server::class::{Behaviour, Context, SpawnResult, NEVER_THINK};
use crate::server::damage::{
    self, DamageInfo, DamageMode, Damaged, LifeState, DMG_PREVENT_PHYSICS_FORCE,
};
use crate::server::entity::EntityCore;
use crate::server::io::{FieldType, Input};
use crate::server::movement::{
    MoveType, Solid, FL_CLIENT, FL_FROZEN, FL_GODMODE, FL_NOTARGET, FL_ONGROUND, FSOLID_NOT_SOLID,
};

use super::{InputDef, InputDefs};

/// `IN_JUMP` (`game/shared/in_buttons.h:12`).
///
/// The server's own copy of two of the `IN_*` bits, because
/// [`PlayerState::buttons`](crate::server::PlayerState::buttons) crosses the
/// seam as a raw mask — `ButtonBits` is a `client/` type and this module names
/// none.
pub const IN_JUMP: u32 = 1 << 1;
/// `IN_DUCK`.
///
/// `IN_SCORE` (1 << 16) and `IN_ZOOM` (1 << 19) are the other two the player
/// reads in the original — both masked out of `PlayerDeathThink`'s "any button
/// down" test — and neither is declared here, because single-player Portal 2
/// never reaches that branch: `sp_fade_and_force_respawn` gets there first.
pub const IN_DUCK: u32 = 1 << 2;

/// `VEC_DUCK_HULL_MAX.z` (`portal_mp_gamerules.cpp:177`) — 36, against 72
/// standing.
///
/// The server has no `m_bDucked`: what crosses the seam is the *hull*
/// ([`PlayerState::mins`](crate::server::PlayerState::mins) and `maxs`,
/// because that is what the touch query sweeps), so "is the player crouched"
/// is a question about its height. The comparison is `<=` against 36 rather
/// than `== 36`, because the duck transition interpolates the eye and not the
/// box: the hull snaps between the two values and never sits between them.
pub const DUCK_HULL_HEIGHT: f32 = 36.0;

/// `sk_dmg_take_scale1`, the factor `CPortal_Player::OnTakeDamage` multiplies
/// **every** hit by (`portal_player.cpp:3607`).
///
/// > **This number is not recoverable and 1 is the only safe value.** The cvar
/// > is declared `extern` in `portal_player.cpp` and defined *nowhere* in this
/// > tree — it belongs to `hl2_gamerules.cpp`, which `CPortalGameRules` derives
/// > from (`portal_gamerules.h:32`) and which the cstrike15 tree does not
/// > contain — and the shipped depot sets it in no `.cfg` and no VPK.
/// > `skill_portal2.cfg` says at the top that it was merged from HL2's and
/// > ep2's "with unknown convars removed", and this is one of the removed.
/// > A cvar nothing sets takes its declared default, which is the value this
/// > tree does not have.
/// >
/// > It barely matters and it is worth knowing why: of the game's 215
/// > `trigger_hurt`s, the *weakest* deals 10 a second against 100 health, and
/// > 202 of them deal 100 or more. Any scale between about 0.1 and 10 kills
/// > the player in the same place. One definition site, one line to change.
const SK_DMG_TAKE_SCALE: f32 = 1.0;

/// `flFadeAndResapwnTime` (`portal_player.cpp:2175`, Valve's spelling) — the
/// single-player death fade, and the real respawn timer.
///
/// The other branch — wait for every button up, then any button down, and not
/// before `PORTAL_RESPAWN_DELAY` (1 s) — is multiplayer's and is unreachable
/// here, which is why neither that constant nor the two button bits it reads
/// are declared.
///
/// `sp_fade_and_force_respawn` defaults to 1, so single-player Portal 2 fades
/// to black over three seconds and then reloads without waiting for a button.
const FADE_AND_RESPAWN_TIME: f32 = 3.0;

/// How often `PlayerDeathThink` runs. `SetNextThink( gpGlobals->curtime + 0.1f )`
/// at the top of it, unconditionally.
const DEATH_THINK_INTERVAL: f32 = 0.1;

/// `CBasePlayer::SharedSpawn`'s `m_iHealth = 100` (`baseplayer_shared.cpp:2415`).
pub const PLAYER_HEALTH: i32 = 100;

/// `CBasePlayer` at stage 5's scope.
///
/// Everything an entity shares lives on its [`EntityCore`] — origin, velocity,
/// flags, health, life state, move type — so what is left here is the three
/// fields that are the *player's* and nothing else's.
#[derive(Debug, Default)]
pub struct Player {
    /// `m_nButtons` — what the client is holding, as `IN_*` bits.
    buttons: u32,
    /// `m_afButtonLast`, and the edges derived from it. Written by
    /// `UpdateButtonState`, read by `logic_playerproxy`.
    last_buttons: u32,
    pressed_buttons: u32,
    /// `m_flDeathTime` — when `Event_Killed` ran. The fade and the respawn are
    /// both measured from it.
    death_time: f32,
    /// `m_fNextSuicideTime` — "don't let them suicide for 5 seconds after
    /// suiciding" (`player.cpp:5578`). Unreachable from map data; the `kill`
    /// console command is its one caller.
    next_suicide_time: f32,
}

/// `DEFINE_INPUTFUNC( FIELD_INTEGER, "SetHealth", InputSetHealth )`
/// (`player.cpp:455`).
///
/// One of the player's three inputs. The other two are `SetHUDVisibility`
/// (there is no HUD) and `SetFogController` (there is no fog), and both were
/// already in the depot test's refused list before this stage.
pub static PLAYER_INPUTS: InputDefs = &[InputDef::new("SetHealth", FieldType::Int)];

/// The player has no map keys: it is never in a `.bsp`.
pub static PLAYER_KEYS: &[&str] = &[];

pub static PLAYER_OUTPUTS: &[&str] = &[];

impl Player {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<Player>::default()
    }

    /// `m_nButtons`, for [`Server::player_state`](crate::server::Server).
    pub fn buttons(&self) -> u32 {
        self.buttons
    }

    /// `m_afButtonPressed` — the bits that went down since the last call.
    pub fn pressed_buttons(&self) -> u32 {
        self.pressed_buttons
    }

    /// `CBasePlayer::UpdateButtonState` (`player.cpp:4030`).
    ///
    /// ```text
    /// m_afButtonLast = m_nButtons;
    /// m_nButtons = nUserCmdButtonMask;
    /// int buttonsChanged = m_afButtonLast ^ m_nButtons;
    /// m_afButtonPressed  =  buttonsChanged & m_nButtons;
    /// m_afButtonReleased =  buttonsChanged & (~m_nButtons);
    /// ```
    ///
    /// `m_afButtonReleased` is not kept: nothing in the port reads it.
    pub fn update_button_state(&mut self, buttons: u32) {
        self.last_buttons = self.buttons;
        self.buttons = buttons;
        let changed = self.last_buttons ^ self.buttons;
        self.pressed_buttons = changed & self.buttons;
    }

    /// `CBasePlayer::CommitSuicide` (`player.cpp:5566`) — the `kill` command.
    ///
    /// The short form, which sets health to zero and calls `Event_Killed`
    /// directly rather than going through `TakeDamage`. Returns whether it
    /// happened; `false` for an already-dead player or one inside the
    /// five-second cooldown.
    pub fn commit_suicide(
        &mut self,
        entity: &mut EntityCore,
        cx: &mut Context<'_>,
        force: bool,
    ) -> bool {
        if !entity.is_alive() {
            return false;
        }
        if self.next_suicide_time > cx.curtime() && !force {
            return false;
        }
        self.next_suicide_time = cx.curtime() + 5.0;
        entity.health = 0;
        let me = entity.id();
        // `DMG_PREVENT_PHYSICS_FORCE | DMG_NEVERGIB` — the flags that say "do
        // not launch the corpse", which matters to a ragdoll this port has not
        // got and is reproduced because the *type* reaches `OnHurt` outputs.
        let info = DamageInfo::new(
            Some(me),
            Some(me),
            0.0,
            DMG_PREVENT_PHYSICS_FORCE | damage::DMG_NEVERGIB,
        );
        self.event_killed(entity, &info, cx);
        self.event_dying(entity, cx);
        true
    }

    /// `CBasePlayer::Event_Dying` (`player.cpp:1861`), which in the original is
    /// a separate virtual that `CBaseCombatCharacter::OnTakeDamage` calls right
    /// after `Event_Killed`.
    ///
    /// **One line survives of six**, and it is the one that matters: start the
    /// death think. The rest is a death sound and a vehicle — and the
    /// `SetLocalAngles` that levels pitch and roll, which is deliberately
    /// *not* here: it operates on the entity's **body** angles, which for a
    /// `CBasePlayer` are yaw-only and separate from the eye, and this port
    /// keeps one angle field that
    /// [`PlayerState`](crate::server::PlayerState) documents as the *view*
    /// angles. Reproducing a body-angle operation on a view-angle field would
    /// be a no-op that reads like a behaviour — `Server::set_player_state`
    /// overwrites it from the client on the very next rendered frame.
    fn event_dying(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        entity.set_next_think(cx.curtime() + DEATH_THINK_INTERVAL, cx);
    }

    /// `CPortal_Player::PlayerDeathThink` (`portal_player.cpp:2097`), the
    /// single-player path.
    ///
    /// Valve's runs every 0.1 s and is mostly branches this port cannot reach:
    /// the crush and gib conditions (multiplayer), `CleansePaint`, the death
    /// animation (`#if 0` in the original — "we're not playing death animations
    /// right now"), `PackDeadPlayerItems`, observer mode and
    /// `mp_forcerespawn`. What is left is the friction that stops a sliding
    /// corpse, the fade, and the respawn.
    ///
    /// > **`sp_fade_and_force_respawn` is 1 and that is the whole of
    /// > single-player death**: fade to black for three seconds, then respawn
    /// > *without* waiting for a button. The `PORTAL_RESPAWN_DELAY` branch
    /// > underneath it — wait for every button up, then any button down — is
    /// > multiplayer's and is unreachable here.
    fn player_death_think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        entity.set_next_think(cx.curtime() + DEATH_THINK_INTERVAL, cx);

        // "if ( GetFlags() & FL_ONGROUND )" — 20 units a tenth of a second of
        // sliding friction, and a hard stop once it is spent.
        if entity.has_flags(FL_ONGROUND) {
            let speed = entity.velocity.length() - 20.0;
            entity.velocity = match speed <= 0.0 {
                true => glam::Vec3::ZERO,
                false => entity.velocity.normalize_or_zero() * speed,
            };
        }

        if entity.life_state == LifeState::Dying {
            entity.life_state = LifeState::Dead;
        }

        // `UTIL_ScreenFade( this, clr, 3, 4, FFADE_OUT | FFADE_STAYOUT )` —
        // not ported, because a screen fade is a user message to a HUD that
        // does not exist. The *timer* it is drawn against is what matters and
        // that is the next three lines.
        if cx.curtime() > self.death_time + FADE_AND_RESPAWN_TIME {
            self.respawn_player(entity, cx);
        }
    }

    /// `CPortal_Player::RespawnPlayer` (`portal_player.cpp:2222`) reduced to
    /// the branch single player takes.
    ///
    /// Valve's calls `respawn( this, … )`, which in single player is
    /// `engine->ServerCommand( "reload\n" )` (`cs_client.cpp:188`) — a *save
    /// game* reload, not a `Spawn`. This port has no saves, so it asks the
    /// engine to start the level again: see
    /// [`Context::reload_level`](crate::server::class::Context::reload_level).
    fn respawn_player(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.buttons = 0;
        entity.set_next_think(NEVER_THINK, cx);
        cx.reload_level();
    }
}

impl Behaviour for Player {
    /// `CBasePlayer::Spawn` (`player.cpp:5097`) and `SharedSpawn`
    /// (`baseplayer_shared.cpp:2405`), reduced to the state this port has.
    ///
    /// The bounding box is *not* set here: it changes when the player ducks, so
    /// it arrives with every other piece of the player's shape through
    /// [`Server::set_player_state`](crate::server::Server::set_player_state).
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        // `SetSolid( SOLID_BBOX )` — and no `FSOLID_NOT_SOLID`, so `IsSolid()`
        // is true, which is what `CTriggerPush::Touch` demands before it will
        // push anything. A respawn has to *clear* the bit `Event_Killed` set.
        entity.solid = Solid::Bbox;
        entity.remove_solid_flags(FSOLID_NOT_SOLID);
        // `AddFlag( FL_CLIENT )` (`player.cpp:5129`). The bit
        // `PassesTriggerFilters` tests against `SF_TRIGGER_ALLOW_CLIENTS`.
        entity.flags |= FL_CLIENT;
        // `SetMoveType( MOVETYPE_WALK )`. **The server's field since stage 5**
        // — `noclip` writes it here and the client reads it back.
        entity.move_type = MoveType::Walk;
        // `m_lifeState = LIFE_ALIVE; m_iHealth = 100; m_takedamage = DAMAGE_YES;`
        // and then `m_iMaxHealth = m_iHealth` in `CBasePlayer::Spawn` itself,
        // which is why 100 appears once.
        entity.life_state = LifeState::Alive;
        entity.health = PLAYER_HEALTH;
        entity.max_health = PLAYER_HEALTH;
        entity.take_damage = DamageMode::Yes;
        SpawnResult::Ok
    }

    fn is_player(&self) -> bool {
        true
    }

    /// `CPortal_Player::OnTakeDamage` (`portal_player.cpp:3548`) over
    /// `CBasePlayer::OnTakeDamage` (`player.cpp:1192`), reduced to the eight
    /// lines that change a number.
    ///
    /// In order, and every one of them is a gate the shipped game has:
    ///
    /// 1. **The Portal scale.** `inputInfoCopy.ScaleDamage( sk_dmg_take_scale1 )`
    ///    — see [`SK_DMG_TAKE_SCALE`].
    /// 2. **`FL_GODMODE`.** The `god` command.
    /// 3. **Zero damage is refused**, before anything else can round it.
    /// 4. **Already dead is refused**, which is what stops a `trigger_hurt`
    ///    running a corpse's health to −4,000 and re-firing `OnHurtPlayer`
    ///    every half second for the rest of the level.
    /// 5. The arithmetic, shared with every other class.
    /// 6. `Event_Killed` and `Event_Dying` at zero.
    ///
    /// Deliberately absent and each measured: the armour block
    /// (`m_ArmorValue` is always 0 — Portal has no armour and no item that
    /// gives any), `m_lastDamageAmount`/`m_DmgTake`/`m_bitsDamageType` (a HUD),
    /// the HEV suit's twenty lines of `SetSuitUpdate` voice lines (Portal has
    /// no suit), the turret and `prop_glados_core` special cases (neither class
    /// exists), the gib and crush conditions (`GameRules()->IsMultiplayer()`),
    /// and `DoAnimationEvent( PLAYERANIMEVENT_FLINCH_CHEST )`.
    fn on_take_damage(
        &mut self,
        entity: &mut EntityCore,
        info: &DamageInfo,
        cx: &mut Context<'_>,
    ) -> Damaged {
        let mut info = *info;
        // "FIXME: This is a hold-over from old Portal behavior -- we should
        // adjust the health to compensate! -- jdw"
        info.scale(SK_DMG_TAKE_SCALE);

        if entity.has_flags(FL_GODMODE) {
            return Damaged::Refused;
        }
        // "Early out if there's no damage" — **before** the fractional
        // accumulator, so a zero never touches it.
        if info.damage == 0.0 {
            return Damaged::Refused;
        }
        // "Already dead" (`player.cpp:1254`).
        if !entity.is_alive() {
            return Damaged::Refused;
        }

        let result = damage::take_damage(
            entity.take_damage,
            &mut entity.health,
            &mut entity.damage_accumulator,
            &info,
        );
        if result == Damaged::Killed {
            self.event_killed(entity, &info, cx);
            self.event_dying(entity, cx);
        }
        result
    }

    /// `CBasePlayer::Event_Killed` (`player.cpp:1786`), reduced to the six
    /// lines that change state a player can observe.
    ///
    /// > **It does not call `UTIL_Remove`**, which is where it stops being
    /// > `CBaseEntity::Event_Killed`: a dead player is still in the entity
    /// > list, still has a position, and still falls over. Deleting it is what
    /// > the default does and what every other class wants.
    fn event_killed(&mut self, entity: &mut EntityCore, _info: &DamageInfo, cx: &mut Context<'_>) {
        // "don't let the status bar glitch for players with <0 health"
        // (`player.cpp:1814`) — and it is the *floor* that matters here, not
        // the status bar: `CGameMovement::IsDead` is `m_iHealth <= 0`, so a
        // health of −4,000 and a health of 0 mean the same thing to the
        // movement, and the clamp is what keeps `ent_dump` readable.
        if entity.health < -99 {
            entity.health = 0;
        }
        entity.life_state = LifeState::Dying;
        // `AddSolidFlags( FSOLID_NOT_SOLID )` — a corpse is walked through,
        // which also takes it out of every trigger's touch query, so the
        // `trigger_hurt` that killed it stops firing `OnHurtPlayer`.
        entity.add_solid_flags(FSOLID_NOT_SOLID);
        // `SetMoveType( MOVETYPE_FLYGRAVITY ); SetGroundEntity( NULL );` —
        // read by `client/`, which runs `FullTossMove` for it.
        entity.move_type = MoveType::FlyGravity;
        entity.flags &= !FL_ONGROUND;
        self.death_time = cx.curtime();
    }

    /// `CBasePlayer::PlayerDeathThink` is the player's only think.
    fn think(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        if entity.is_alive() {
            return;
        }
        self.player_death_think(entity, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if input.name.eq_ignore_ascii_case("SetHealth") {
            // `CBasePlayer::InputSetHealth` (`player.cpp:8806`) — *not* a field
            // write. It computes the difference and routes it through
            // `TakeHealth` or `TakeDamage`, so setting a player's health to
            // zero from a map kills them properly.
            let target = input.value.int();
            let delta = (entity.health - target).abs() as f32;
            if target > entity.health {
                damage::take_health(
                    entity.take_damage,
                    &mut entity.health,
                    entity.max_health,
                    delta,
                );
            } else if target < entity.health {
                // **Not `cx.take_damage`.** That queues, and the queue is
                // keyed by handle — and this entity is *detached* for the
                // duration of its own handler, so a self-aimed queue entry
                // would resolve to nothing and be dropped. Hurting yourself is
                // the one case that does not need the queue at all: both
                // borrows are already in hand, which is exactly what the C++'s
                // `TakeDamage( ... )` on `this` compiles to.
                let me = entity.id();
                let info = DamageInfo::new(Some(me), Some(me), delta, damage::DMG_GENERIC);
                self.on_take_damage(entity, &info, cx);
            }
            return true;
        }
        false
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("m_nButtons", format!("{:#x}", self.buttons)),
            ("m_flDeathTime", format!("{:.2}", self.death_time)),
        ]
    }
}

// ---------------------------------------------------------------------------
// logic_playerproxy
// ---------------------------------------------------------------------------

/// `CLogicPlayerProxy` (`logic_playerproxy.cpp:20`) — "used to relay
/// outputs/inputs from the player to the world and vice versa".
///
/// **Nine in the game, across eight maps, and every one of the five output
/// connections in the whole game is on `sp_a1_intro1`** — the map this port
/// loads by default. Jumping there fires three relays and ducking fires two,
/// which makes this the first class in the port whose *input* is the person
/// playing rather than the map.
///
/// # Portal 2 has fewer of these than the header suggests
///
/// `logic_playerproxy.h` declares eighteen outputs and twenty inputs across
/// three `#ifdef` families, and Portal 2 compiles two of them. What that
/// leaves, measured against the datadesc rather than the header:
///
/// - **Every input this class has in Portal 2 is a portal-gun or grab-controller
///   input** — `AddPotatosToPortalgun`, `ForceVMGrabController`,
///   `SetMotionBlurAmount` and five more, 20 connections in the shipped maps —
///   and not one of them is portable without the portal system. So this class
///   accepts **no inputs at all**, which is the honest shape rather than a gap.
/// - **`RequestPlayerHealth` and `SetPlayerHealth` are HL2-only**
///   (`#if defined HL2_EPISODIC && !defined( PORTAL2 )`), so the `PlayerHealth`
///   output they exist to fire **cannot fire in Portal 2**. It is declared in
///   the base block and is dead.
/// - **`PlayerDied` is declared and fired by nothing**, in the entire tree.
///   Searching for it finds one hit and it is a *Squirrel* function name
///   (`portal_player.cpp:3535`, `RunScript( ..., "PlayerDied" )`), which is a
///   different thing that happens to share the string.
///
/// So three outputs are live — `OnJump`, `OnDuck`, `OnUnDuck` — and three are
/// what the maps connect.
#[derive(Debug, Default)]
pub struct LogicPlayerProxy;

pub static PLAYER_PROXY_KEYS: &[&str] = &[];

/// See the type docs: Portal 2's are all portal-gun inputs.
pub static PLAYER_PROXY_INPUTS: InputDefs = &[];

pub static PLAYER_PROXY_OUTPUTS: &[&str] = &[
    // Live.
    "OnJump",
    "OnDuck",
    "OnUnDuck",
    // Declared by the class and unfireable here — see the type docs. They are
    // listed because `ClassDef::outputs` is what makes a key an *output*
    // rather than a key nothing understood, and a map is allowed to connect
    // one.
    "PlayerDied",
    "PlayerHealth",
    "OnStartSlowingTime",
    "OnStopSlowingTime",
    "OnPrimaryPortalPlaced",
    "OnSecondaryPortalPlaced",
    "OnCoopPing",
];

impl LogicPlayerProxy {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<LogicPlayerProxy>::default()
    }
}

impl Behaviour for LogicPlayerProxy {}

// ---------------------------------------------------------------------------
// player_loadsaved
// ---------------------------------------------------------------------------

/// `CRevertSaved` (`player.cpp:7954`) — **Portal 2's other way of dying**.
///
/// Nine in the game across eight maps, seven of them named some variation of
/// `fade_to_death`, and 11 connections fire `Reload` at them. It is what
/// happens when you fall into the abyss in `sp_a3_portal_intro` or off the
/// track in `sp_a3_speed_ramp`: there is no `trigger_hurt` down there, so
/// nothing takes any health — the map freezes you, fades the screen and
/// reloads.
///
/// # It reloads a *save*, and this port has none
///
/// `LoadThink` is `engine->ServerCommand( "reload\n" )`, the same line
/// single-player `respawn()` ends in, and it restores the last save game.
/// `portdocs/SERVER.md` §6 defers save/restore — as `serde` over the entity
/// state rather than a port of `ISave`/`IRestore` — so this asks the engine to
/// start the level again instead. For a Portal 2 chamber the two are usually
/// the same place, because the game autosaves on entry.
///
/// The screen fade is not ported: `UTIL_ScreenFadeAll` is a user message to a
/// HUD that does not exist. What *is* ported is the state it is drawn over —
/// `FL_FROZEN|FL_NOTARGET` on the player and `deadflag`, so you cannot walk
/// away during the two and a half seconds it takes.
#[derive(Debug, Default)]
pub struct RevertSaved {
    /// `m_loadTime` — how long to wait before reloading. Every shipped one is
    /// 2 or 2.5.
    load_time: f32,
    /// `m_Duration`/`m_HoldTime` — the fade's shape. Parsed so the keys are
    /// consumed and read by nothing, because there is no fade.
    duration: f32,
    hold_time: f32,
}

pub static REVERT_SAVED_KEYS: &[&str] = &["loadtime", "duration", "holdtime"];

pub static REVERT_SAVED_INPUTS: InputDefs = &[InputDef::new("Reload", FieldType::Void)];

pub static REVERT_SAVED_OUTPUTS: &[&str] = &[];

impl RevertSaved {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<RevertSaved>::default()
    }
}

impl Behaviour for RevertSaved {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        let is = |name: &str| key.eq_ignore_ascii_case(name);
        if is("loadtime") {
            self.load_time = crate::server::keyvalue::atof(value);
            return true;
        }
        if is("duration") {
            self.duration = crate::server::keyvalue::atof(value);
            return true;
        }
        if is("holdtime") {
            self.hold_time = crate::server::keyvalue::atof(value);
            return true;
        }
        false
    }

    /// `CRevertSaved::InputReload` (`player.cpp:8035`).
    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        if !input.name.eq_ignore_ascii_case("Reload") {
            return false;
        }
        // `UTIL_ScreenFadeAll( m_clrRender, Duration(), HoldTime(), FFADE_OUT )`
        // — no HUD, no fade. The colour is `rendercolor` and is parsed by the
        // base ladder; seven of the nine are a dark brown.
        entity.set_next_think(cx.curtime() + self.load_time, cx);
        // "Adrian: Setting this flag so we can't move or save a game."
        // `pl.deadflag = true` as well, which here is the life state — but
        // *not* `Event_Killed`, so the player keeps its health and its move
        // type and simply cannot act.
        if let Some(player) = cx.player() {
            if let Some(core) = cx.entity_mut(player) {
                core.flags |= FL_NOTARGET | FL_FROZEN;
            }
        }
        true
    }

    /// `CRevertSaved::LoadThink` (`player.cpp:8056`).
    fn think(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        cx.reload_level();
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("loadtime", format!("{:.2}", self.load_time)),
            ("duration", format!("{:.2}", self.duration)),
            ("holdtime", format!("{:.2}", self.hold_time)),
        ]
    }
}
