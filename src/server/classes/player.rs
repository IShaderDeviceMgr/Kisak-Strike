//! The player, as far as the entity system is concerned.
//!
//! `CBasePlayer` (`game/server/player.cpp`, 9,940 lines) and
//! `CPortal_Player` (`game/server/portal/portal_player.cpp`), reduced to the
//! ~60 lines that make the sentence "the player touched a trigger" mean
//! something.
//!
//! # This is the half of stage 5 that stage 4 could not do without
//!
//! `portdocs/SERVER.md` puts "the player as an entity" at stage 5 and triggers
//! at stage 4, and stage 4 ends with "walking through a trigger fires its
//! outputs". Those two cannot both be true: a touch is a fact about *two*
//! entities, `PassesTriggerFilters` tests `FL_CLIENT` on the toucher,
//! `trigger_hurt` chooses its output by `IsPlayer()`, `filter_activator_name`
//! has a special case for the literal string `!player`, and **121 of the
//! game's 128 `point_teleport`s target `!player`**. Building the touch system
//! against something that is not in the entity list would have been building a
//! different system.
//!
//! So the player is an entity here, and stage 5 is what is *left*: the
//! movement itself, `noclip`'s home (`portdocs/CLIENT.md` §9.2), health and
//! death, the weapon, the view. **This class holds none of that and no
//! `client/` type.** It is a box with `FL_CLIENT` set, whose position arrives
//! once a tick as [`PlayerState`](crate::server::PlayerState) — the same
//! plain-value seam `world/` gets for brush placements, pointing the other
//! way.
//!
//! # What it is not
//!
//! It does not move: [`MoveType::Walk`] is a *label* on the server side and
//! `movement::simulate` runs no movement for it, because
//! `Client::run_move` already did on the rendered frame. It has no health,
//! because there is no damage. It has no weapon, no view model, no suit, no
//! team. Every one of those is stage 5's or later, and each would be visible
//! as a missing field rather than as wrong behaviour.

use crate::server::class::{Behaviour, Context, SpawnResult};
use crate::server::entity::EntityCore;
use crate::server::movement::{MoveType, Solid, FL_CLIENT};

/// `CBasePlayer`, at stage 4's scope. See the module docs.
///
/// Stateless: everything about the player that this module can see lives on
/// its [`EntityCore`], because every field of it is either shared with every
/// other entity (`origin`, `velocity`, `flags`) or is refreshed from
/// `client/` every tick. The struct exists to carry
/// [`is_player`](Behaviour::is_player), which is the one virtual three other
/// classes ask about.
pub struct Player;

impl Player {
    pub fn create() -> Box<dyn Behaviour> {
        Box::new(Player)
    }
}

impl Behaviour for Player {
    /// `CBasePlayer::Spawn` (`player.cpp:1063`), reduced to the four lines
    /// that decide how the player interacts with a trigger.
    ///
    /// The bounding box is *not* set here: it changes when the player ducks,
    /// so it arrives with every other piece of the player's state through
    /// [`Server::set_player_state`](crate::server::Server::set_player_state).
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        // `SetSolid( SOLID_BBOX )` — and no `FSOLID_NOT_SOLID`, so
        // `IsSolid()` is true, which is what `CTriggerPush::Touch` demands
        // before it will push anything.
        entity.solid = Solid::Bbox;
        // `AddFlag( FL_CLIENT )` (`player.cpp:1075`). The bit
        // `PassesTriggerFilters` tests against `SF_TRIGGER_ALLOW_CLIENTS`.
        entity.flags |= FL_CLIENT;
        // `SetMoveType( MOVETYPE_WALK )`. Overwritten every tick by whatever
        // `client/` says, `noclip` included.
        entity.move_type = MoveType::Walk;
        // `m_takedamage = DAMAGE_YES` (`player.cpp:1101`). Read by exactly one
        // line, `CTriggerHurt::HurtEntity`'s first test — and if it were
        // `false` a `trigger_hurt` would stop thinking after one tick and
        // never fire `OnHurtPlayer`.
        entity.take_damage = true;
        SpawnResult::Ok
    }

    fn is_player(&self) -> bool {
        true
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }
}
