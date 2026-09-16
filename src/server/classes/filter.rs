//! The `filter_*` family: the answer to "does *this* one count?".
//!
//! `game/server/filters.cpp` (`CBaseFilter` and the eleven classes on it),
//! of which Portal 2 places six — **302 entities**:
//!
//! ```text
//!    212  filter_activator_class    almost all of them "prop_weighted_cube"
//!     74  filter_activator_name
//!      9  filter_multi
//!      4  filter_player_held
//!      2  filter_damage_type
//!      1  filter_activator_model
//! ```
//!
//! A filter is never triggered and never fires on its own: it is a question
//! another entity asks. 318 of the game's triggers name one in `filtername`,
//! and what those filters mostly say is **"cubes only"** — which is why
//! implementing them is not optional decoration for stage 4. Without the
//! filter, 250 of the game's 899 `trigger_multiple`s would fire for a player
//! walking past a dropper that is meant to notice only its own cube.
//!
//! # `Negated` is applied by each class, not by the caller
//!
//! `CBaseFilter` splits the work in two: `PassesFilterImpl` is the criterion
//! and `PassesFilter` is `m_bNegated ? !Impl() : Impl()`. The split exists for
//! `filter_multi`, which combines the *already negated* results of its
//! children and then negates its own combination. Here there is one method,
//! [`Behaviour::passes_filter`], and every class ends with
//! [`BaseFilter::negate`] — same result, and the one place it matters
//! (`filter_multi` calling `Filters::passes`, which goes through the
//! children's own `passes_filter`) is preserved exactly.
//!
//! # The value of `Negated` in a shipped map is not a number
//!
//! Hammer writes the *label* of the chosen `choices` entry, so 289 of the 302
//! filters in the game carry
//! `"Negated" "Allow entities that match criteria"`. `atoi` of that is 0,
//! which is the right answer — and is the reason `keyvalue::atoi` exists
//! rather than `str::parse` (`rustdocs/SERVER.md` gotcha 14). Three
//! `filter_activator_name`s carry a literal `1`.

use crate::server::class::{Behaviour, Context, Filters, InputDef, InputDefs};
use crate::server::damage::{DamageInfo, DMG_DIRECT};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input, Variant};
use crate::server::keyvalue::atoi;
use crate::server::name;

/// `CBaseFilter`'s own state — held, not inherited, by all six classes.
#[derive(Default)]
pub struct BaseFilter {
    /// `m_bNegated` — the `Negated` key. See the module docs for what a map
    /// actually writes here.
    pub negated: bool,
}

impl BaseFilter {
    /// `CBaseFilter::KeyValue`'s one key.
    pub fn key_value(&mut self, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("Negated") {
            self.negated = atoi(value) != 0;
            return true;
        }
        false
    }

    /// The second half of `CBaseFilter::PassesFilter`.
    pub fn negate(&self, result: bool) -> bool {
        match self.negated {
            true => !result,
            false => result,
        }
    }

    /// `CBaseFilter::InputTestActivator` (`filters.cpp:64`) — run the filter
    /// against the activator and say so.
    ///
    /// Zero shipped connections fire it. It is here because it is the class's
    /// only input and because it is the cheapest way to test a filter from
    /// outside the touch path.
    pub fn test_activator(
        entity: &mut EntityCore,
        behaviour: &dyn Behaviour,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        let passed = input
            .activator
            .and_then(|id| cx.entity(id).map(|e| &e.core))
            .map(|other| behaviour.passes_filter(entity, other, &cx.filters()))
            // A null activator fails: `PassesFilter( pCaller, NULL )` reaches
            // `pEntity->NameMatches` and friends on a null pointer in the
            // original, which is a crash rather than a decision. Refusing is
            // the nearest thing to a behaviour it has.
            .unwrap_or(false);

        let me = entity.id();
        let output = match passed {
            true => "OnPass",
            false => "OnFail",
        };
        entity.fire_output(output, Variant::Void, input.activator, Some(me), 0.0, cx);
        true
    }
}

/// The inputs and outputs every filter has.
pub static FILTER_INPUTS: InputDefs = &[
    // `FIELD_INPUT`: the value is not used at all — `InputTestActivator` reads
    // the *activator*, not the parameter — so the declared type is the one
    // that converts nothing.
    InputDef::new("TestActivator", FieldType::Input),
];

pub static FILTER_OUTPUTS: &[&str] = &["OnPass", "OnFail"];

/// The key every filter has, for the classes whose own list is otherwise
/// empty.
pub static BASE_FILTER_KEYS: &[&str] = &["Negated"];

// ---------------------------------------------------------------------------
// filter_activator_name
// ---------------------------------------------------------------------------

/// `CFilterName` (`filters.cpp:245`) — 74 entities. Matches the activator's
/// `targetname`.
#[derive(Default)]
pub struct FilterName {
    base: BaseFilter,
    /// `m_iFilterName` — confusingly, the *name to match*, not this filter's
    /// own name. Valve's spelling of the key is `filtername`, the same key
    /// a trigger uses to name a filter, on a different class.
    filter_name: Option<String>,
}

pub static FILTER_NAME_KEYS: &[&str] = &["Negated", "filtername"];

impl FilterName {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<FilterName>::default()
    }
}

impl Behaviour for FilterName {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("filtername") {
            self.filter_name = Some(value.to_owned());
            return true;
        }
        self.base.key_value(key, value)
    }

    fn is_filter(&self) -> bool {
        true
    }

    /// `CFilterName::PassesFilterImpl` (`filters.cpp:253`).
    ///
    /// > **The literal string `!player` is a special case**, and Valve says
    /// > why in a comment: `GetEntityName` for the player does not return
    /// > `"!player"`, so the ordinary name match could never succeed. It is
    /// > the only procedural name any filter understands. Six of the game's
    /// > 74 use it.
    fn passes_filter(&self, _entity: &EntityCore, other: &EntityCore, _f: &Filters<'_>) -> bool {
        let Some(want) = self.filter_name.as_deref() else {
            // `NameMatches( NULL )` — `FStrEq` against an empty string, which
            // matches only an unnamed entity.
            return self.base.negate(other.name.is_none());
        };
        let matched = match want.eq_ignore_ascii_case("!player") {
            true => other.has_flags(crate::server::movement::FL_CLIENT),
            false => other
                .name
                .as_deref()
                .is_some_and(|have| name::names_match(want, have)),
        };
        self.base.negate(matched)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        test_activator(self, entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("filtername", format!("{:?}", self.filter_name)),
            ("Negated", self.base.negated.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// filter_activator_class
// ---------------------------------------------------------------------------

/// `CFilterClass` (`filters.cpp:333`) — 212 entities and the commonest filter
/// in the game.
///
/// 160 of them name `prop_weighted_cube`, which this port has no class for, so
/// they refuse everything — and that is the *right* answer: a cube-only
/// trigger should not fire for a player.
#[derive(Default)]
pub struct FilterClass {
    base: BaseFilter,
    filter_class: Option<String>,
}

pub static FILTER_CLASS_KEYS: &[&str] = &["Negated", "filterclass"];

impl FilterClass {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<FilterClass>::default()
    }
}

impl Behaviour for FilterClass {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("filterclass") {
            self.filter_class = Some(value.to_owned());
            return true;
        }
        self.base.key_value(key, value)
    }

    fn is_filter(&self) -> bool {
        true
    }

    /// `pEntity->ClassMatches( STRING(m_iFilterClass) )`, which is the same
    /// wildcard comparison [`names_match`](name::names_match) does for
    /// targetnames (`baseentity.cpp:684`).
    fn passes_filter(&self, _entity: &EntityCore, other: &EntityCore, _f: &Filters<'_>) -> bool {
        let matched = self
            .filter_class
            .as_deref()
            .is_some_and(|want| name::names_match(want, other.classname()));
        self.base.negate(matched)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        test_activator(self, entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("filterclass", format!("{:?}", self.filter_class)),
            ("Negated", self.base.negated.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// filter_activator_model
// ---------------------------------------------------------------------------

/// `CFilterModel` (`filters.cpp:279`) — one entity in the whole game, filtering
/// on `models/props/futbol.mdl`.
///
/// > **Its key is `model`, which every other entity in the game uses for its
/// > own model.** Valve gets away with it because `CBaseEntity::KeyValue`'s
/// > if-ladder has no `model` case — the shared `m_ModelName` is a *datadesc*
/// > key, and `ParseKeyvalue` walks derived before base, so the derived
/// > `m_iFilterModel` wins. This port's order (class first, then the ladder)
/// > reaches the same answer by a different route, and the entity's own
/// > [`model`](EntityCore::model) is left `None` in both.
#[derive(Default)]
pub struct FilterModel {
    base: BaseFilter,
    filter_model: Option<String>,
}

pub static FILTER_MODEL_KEYS: &[&str] = &["Negated", "model"];

impl FilterModel {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<FilterModel>::default()
    }
}

impl Behaviour for FilterModel {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("model") {
            self.filter_model = Some(value.to_owned());
            return true;
        }
        self.base.key_value(key, value)
    }

    fn is_filter(&self) -> bool {
        true
    }

    /// `FStrEq( STRING(m_iFilterModel), STRING(pEntity->GetModelName()) )` —
    /// an exact case-insensitive compare, **not** a wildcard match, which is
    /// the one place this family differs from the name and class filters.
    fn passes_filter(&self, _entity: &EntityCore, other: &EntityCore, _f: &Filters<'_>) -> bool {
        let want = self.filter_model.as_deref().unwrap_or("");
        let have = other.model.as_deref().unwrap_or("");
        self.base.negate(want.eq_ignore_ascii_case(have))
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        test_activator(self, entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("model", format!("{:?}", self.filter_model)),
            ("Negated", self.base.negated.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// filter_damage_type
// ---------------------------------------------------------------------------

/// `FilterDamageType` (`filters.cpp:420`) — two entities.
///
/// > **As an *activator* filter it always passes**, and that is not a
/// > simplification: `PassesFilterImpl` is `ASSERT( false ); return true;`.
/// > The real test is `PassesDamageFilterImpl`, which only a `m_hDamageFilter`
/// > reaches, and nothing in this port has one because nothing takes damage.
/// > So both of the game's two pass everything — which, since both carry
/// > `Negated` = allow, is also what the shipped game does.
#[derive(Default)]
pub struct FilterDamageType {
    base: BaseFilter,
    /// `m_iDamageType`. Parsed so the key is consumed and read by nothing.
    damage_type: i32,
}

pub static FILTER_DAMAGE_TYPE_KEYS: &[&str] = &["Negated", "damagetype"];

impl FilterDamageType {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<FilterDamageType>::default()
    }
}

impl Behaviour for FilterDamageType {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("damagetype") {
            self.damage_type = atoi(value);
            return true;
        }
        self.base.key_value(key, value)
    }

    fn is_filter(&self) -> bool {
        true
    }

    /// `ASSERT( false ); return true;` (`filters.cpp:428`) — **as an
    /// *activator* filter this class always passes**, and it is not a
    /// simplification: `FilterDamageType::PassesFilterImpl` is an assert.
    /// Both of the game's two carry `Negated` = allow, so both pass
    /// everything, which is what the shipped game does.
    fn passes_filter(&self, _entity: &EntityCore, _other: &EntityCore, _f: &Filters<'_>) -> bool {
        self.base.negate(true)
    }

    /// `FilterDamageType::PassesDamageFilterImpl` (`filters.cpp:432`) — the
    /// real test, and the reason the class exists.
    ///
    /// > **`==`, not `&`.** The damage type must match *exactly* once
    /// > `DMG_DIRECT` is masked off, so a filter for `DMG_BURN` refuses
    /// > `DMG_BURN|DMG_SLOWBURN`. That is Valve's and it is the sort of thing
    /// > a port "fixes" into a bitmask test without noticing.
    ///
    /// Reachable since `portdocs/SERVER.md` stage 5 gave the port a
    /// `m_hDamageFilter` to hang it off; before that the method existed and
    /// nothing could call it. The game's two are on `sp_a2_bts4`
    /// (`DMG_BURN`) and `sp_a4_finale3` (`DMG_SONIC`), and **neither is named
    /// by any entity's `damagefilter` key** — both are connected to nothing at
    /// all, which is a mapper leaving scaffolding in.
    fn passes_damage_filter(
        &self,
        _entity: &EntityCore,
        info: &DamageInfo,
        _cx: &Context<'_>,
    ) -> bool {
        self.base
            .negate((info.damage_type & !DMG_DIRECT) == self.damage_type)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        test_activator(self, entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("damagetype", self.damage_type.to_string()),
            ("Negated", self.base.negated.to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// filter_player_held
// ---------------------------------------------------------------------------

/// `CFilterPlayerHeld` (`filters.cpp:758`) — four entities, all of them part
/// of the same Hammer instance that asks "is this the sphere the player is
/// carrying?".
///
/// > **It answers `false` here, always**, because the test is
/// > `VPhysicsGetObject()->GetGameFlags() & FVPHYSICS_PLAYER_HELD` and there
/// > is no `vphysics` and no grab controller. That is the same answer the
/// > shipped game gives for anything the player is not holding, which is
/// > everything this port can produce: the entity it is meant to notice is a
/// > `prop_physics`.
#[derive(Default)]
pub struct FilterPlayerHeld {
    base: BaseFilter,
}

impl FilterPlayerHeld {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<FilterPlayerHeld>::default()
    }
}

impl Behaviour for FilterPlayerHeld {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        self.base.key_value(key, value)
    }

    fn is_filter(&self) -> bool {
        true
    }

    fn passes_filter(&self, _entity: &EntityCore, _other: &EntityCore, _f: &Filters<'_>) -> bool {
        self.base.negate(false)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        test_activator(self, entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![("Negated", self.base.negated.to_string())]
    }
}

// ---------------------------------------------------------------------------
// filter_multi
// ---------------------------------------------------------------------------

/// How many sub-filters a `filter_multi` declares. Valve's `MAX_FILTERS`.
const MAX_FILTERS: usize = 10;

/// `filter_t` (`filters.cpp:81`) — how a `filter_multi` combines its children.
const FILTER_AND: i32 = 0;

/// `CFilterMultiple` (`filters.cpp:90`) — nine entities, and the only class
/// here that reads another entity to answer.
///
/// > **The resolved list is compacted and the names are not.** Valve's
/// > `Activate` walks `m_iFilterName[0..10]` and appends each *valid* filter
/// > at `nNextFilter++`, so a `Filter03` that names something which is not a
/// > filter leaves `Filter04` in slot 2 rather than in slot 3. The comment
/// > says why — "we want the array of valid filters to be contiguous" — and it
/// > matters for `FILTER_AND`, whose loop would otherwise stop early on the
/// > hole. Reproduced by building a `Vec`.
#[derive(Default)]
pub struct FilterMulti {
    base: BaseFilter,
    /// `m_nFilterType` — 0 is AND, 1 is OR. Four of the nine are AND.
    filter_type: i32,
    /// `m_iFilterName[10]`, as written.
    names: [Option<String>; MAX_FILTERS],
    /// `m_hFilter[10]`, resolved and compacted at `Activate`.
    filters: Vec<EntityId>,
}

pub static FILTER_MULTI_KEYS: &[&str] = &[
    "Negated",
    "FilterType",
    "Filter01",
    "Filter02",
    "Filter03",
    "Filter04",
    "Filter05",
    "Filter06",
    "Filter07",
    "Filter08",
    "Filter09",
    "Filter10",
];

impl FilterMulti {
    pub fn create() -> Box<dyn Behaviour> {
        Box::<FilterMulti>::default()
    }
}

impl Behaviour for FilterMulti {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        if key.eq_ignore_ascii_case("FilterType") {
            self.filter_type = atoi(value);
            return true;
        }
        for (i, slot) in self.names.iter_mut().enumerate() {
            if key.eq_ignore_ascii_case(&format!("Filter{:02}", i + 1)) {
                *slot = Some(value.to_owned());
                return true;
            }
        }
        self.base.key_value(key, value)
    }

    /// `CFilterMultiple::Activate` (`filters.cpp:133`).
    fn activate(&mut self, _entity: &mut EntityCore, cx: &mut Context<'_>) {
        let filters = cx.filters();
        self.filters = self
            .names
            .iter()
            .flatten()
            .filter_map(|name| filters.find(name))
            .collect();
    }

    fn is_filter(&self) -> bool {
        true
    }

    /// `CFilterMultiple::PassesFilterImpl` (`filters.cpp:166`).
    ///
    /// An AND with no children passes and an OR with none fails, which falls
    /// out of the loops and is Valve's.
    fn passes_filter(&self, entity: &EntityCore, other: &EntityCore, f: &Filters<'_>) -> bool {
        let result = match self.filter_type == FILTER_AND {
            true => self.filters.iter().all(|&id| f.passes(id, entity, other)),
            false => self.filters.iter().any(|&id| f.passes(id, entity, other)),
        };
        self.base.negate(result)
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        test_activator(self, entity, input, cx)
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "FilterType",
                match self.filter_type == FILTER_AND {
                    true => String::from("AND"),
                    false => String::from("OR"),
                },
            ),
            ("filters", self.filters.len().to_string()),
            ("Negated", self.base.negated.to_string()),
        ]
    }
}

/// `TestActivator`'s dispatch, shared by all six classes.
///
/// A free function rather than a default method because it needs `&dyn
/// Behaviour` for the class's *own* `passes_filter` while `accept_input` holds
/// `&mut self` — the same reason `base_accept_input` takes the behaviour as an
/// argument.
fn test_activator<T: Behaviour>(
    filter: &mut T,
    entity: &mut EntityCore,
    input: &Input<'_>,
    cx: &mut Context<'_>,
) -> bool {
    if !input.name.eq_ignore_ascii_case("TestActivator") {
        return false;
    }
    // Split so that the immutable borrow for `passes_filter` and the mutable
    // one for `fire_output` do not overlap: the answer is computed first.
    BaseFilter::test_activator(entity, &*filter, input, cx)
}
