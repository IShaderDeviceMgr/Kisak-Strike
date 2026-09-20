//! The entity classes, and the table that turns a classname into one.
//!
//! `EntityFactoryDictionary` (`game/server/entitylist.cpp:60`) and the
//! `LINK_ENTITY_TO_CLASS`/`BEGIN_DATADESC` pairs scattered through
//! `game/server/`.
//!
//! # What is here, and what it covers
//!
//! **Forty-six classnames, 34,823 of the shipped game's 60,925 entity
//! blocks.** Five of the 46 are placed by no map: `player` (the engine makes
//! it when a client connects), `trigger_portal_button` (a `prop_floor_button`
//! makes it in its own `Spawn`), and `dynamic_prop`, `prop_dynamic_glow` and
//! `light_glspot`, each registered because Valve registers it — so **41 of
//! the 200 classnames the maps place** are implemented.
//!
//! Stage 1 brought ten of them — `worldspawn`, the light family,
//! `info_target`, `info_player_start`, `logic_relay` and
//! `func_instance_io_proxy` — as keyvalue bags with one piece of behaviour
//! between them. Stage 2 gave those ten their behaviour and added six more:
//! `logic_auto`, `logic_branch`, `logic_case`, `logic_timer`, `math_counter`
//! and `env_tonemap_controller`. Stage 3 adds the brush family — `func_brush`,
//! `func_door`, `func_door_rotating`, `func_movelinear`, `func_button` and
//! `func_rotating`, 3,410 entities — and with it everything in a map that
//! moves. Stage 4 adds five triggers, six filters and `point_teleport`, 3,322
//! more, and with them everything in a map that *notices* you.
//! `prop_floor_button` is 65 on its own, and is the first class from
//! `game/server/portal2/`. Stage 5 adds two — `logic_playerproxy` (9) and
//! `player_loadsaved` (9) — which are the map's interface to the player and
//! Portal 2's second way of dying. After the five stages came `prop_dynamic`
//! and its three siblings — 8,462 entities, more than stages 3 and 4 together
//! — and `prop_testchamber_door`, which is 138 across 71 maps and is the
//! chamber door itself. `logic_branch_listener` (158, across 46 maps) came
//! after it, because it is what shuts those doors again. `prop_portal` (21,
//! across 10 maps) is the newest, and is `portdocs/PORTAL.md` stage 2: it
//! links to its partner and computes the teleport matrix, but carves no hole
//! and teleports nobody.
//!
//! The additions are not chosen by instance count alone — `logic_case` is 84
//! entities and `logic_playerproxy` is 9 — but by what a map needs in order to
//! *run*: `logic_auto` is how every map in the game starts itself,
//! `env_tonemap_controller` is how 105 of the 106 say how bright they should
//! be, `func_brush` is the third commonest classname in the game, and **every
//! one of the five `logic_playerproxy` output connections in the entire game
//! is on `sp_a1_intro1`**, which is the map this port loads by default.
//!
//! # One file per family
//!
//! `portdocs/SERVER.md` §7.1 lays out a `classes/` directory with a file per
//! family, and stage 1 said it would split "when stage 2's logic family
//! arrives". It has.

pub mod brush;
pub mod env;
pub mod filter;
pub mod light;
pub mod logic;
pub mod player;
pub mod point;
pub mod portal;
pub mod prop;
pub mod trigger;
pub mod world;

use crate::server::class::{ClassDef, InputDef, InputDefs, PointEntity};
use crate::server::io::FieldType;

pub use brush::{AreaPortal, Brush, Button, Door, MoveLinear, Rotating};
pub use env::TonemapController;
pub use filter::{
    FilterClass, FilterDamageType, FilterModel, FilterMulti, FilterName, FilterPlayerHeld,
};
pub use light::{EnvLight, Light};
pub use logic::{Auto, Branch, BranchList, Case, InstanceIoProxy, MathCounter, Relay, Timer};
pub use player::{
    LogicPlayerProxy, Player, RevertSaved, DUCK_HULL_HEIGHT, IN_DUCK, IN_JUMP, IN_USE,
};
pub use point::PointTeleport;
pub use portal::PropPortal;
pub use prop::{ButtonTrigger, DynamicProp, FloorButton, TestChamberDoor, WeightedCube};
pub use trigger::{TriggerHurt, TriggerMultiple, TriggerPush, TriggerTeleport};
pub use world::World;

/// Every class this port knows. `CEntityFactoryDictionary::m_Factories`.
///
/// Ordered as `portdocs/SERVER.md` §1.2 orders the census — commonest first —
/// because [`lookup`] is a linear scan and because it makes the coverage
/// obvious to a reader.
pub(super) static CLASSES: &[ClassDef] = &[
    ClassDef {
        name: "logic_relay",
        keys: &["StartDisabled"],
        inputs: RELAY_INPUTS,
        outputs: &["OnTrigger", "OnSpawn"],
        create: Relay::create,
    },
    // `CDynamicProp`, four ways (`props.cpp:1914`). Second only to
    // `logic_relay` by ten entities, and the class carries more shipped input
    // connections than any other in the port.
    //
    // **The classname is behaviour, not decoration**: `CDynamicProp::Spawn`
    // asks `FClassnameIs` twice, so a `prop_dynamic_override` keeps
    // `SOLID_NONE` where a `prop_dynamic` is promoted to `SOLID_OBB`, and it
    // is the only one of the four a map may give `health` to. See
    // [`DynamicProp::is_plain_dynamic`] and [`DynamicProp::allows_health`].
    ClassDef {
        name: "prop_dynamic",
        keys: prop::DYNAMIC_PROP_KEYS,
        inputs: DYNAMIC_PROP_INPUTS,
        outputs: prop::DYNAMIC_PROP_OUTPUTS,
        create: DynamicProp::create,
    },
    ClassDef {
        name: "prop_dynamic_override",
        keys: prop::DYNAMIC_PROP_KEYS,
        inputs: DYNAMIC_PROP_INPUTS,
        outputs: prop::DYNAMIC_PROP_OUTPUTS,
        create: DynamicProp::create,
    },
    // The two `LINK_ENTITY_TO_CLASS` names no shipped map uses, registered for
    // the reason `light_glspot` is: a map that placed one would otherwise get
    // an entity the game would not. `dynamic_prop` renames itself to
    // `prop_dynamic` in its own `Spawn`, which is what puts it on the
    // promoted side of the solidity test; `prop_dynamic_glow` does not.
    ClassDef {
        name: "dynamic_prop",
        keys: prop::DYNAMIC_PROP_KEYS,
        inputs: DYNAMIC_PROP_INPUTS,
        outputs: prop::DYNAMIC_PROP_OUTPUTS,
        create: DynamicProp::create,
    },
    ClassDef {
        name: "prop_dynamic_glow",
        keys: prop::DYNAMIC_PROP_KEYS,
        inputs: DYNAMIC_PROP_INPUTS,
        outputs: prop::DYNAMIC_PROP_OUTPUTS,
        create: DynamicProp::create,
    },
    ClassDef {
        name: "light",
        keys: light::LIGHT_KEYS,
        inputs: LIGHT_INPUTS,
        outputs: &[],
        create: Light::create,
    },
    ClassDef {
        name: "light_spot",
        keys: light::LIGHT_KEYS,
        inputs: LIGHT_INPUTS,
        outputs: &[],
        create: Light::create,
    },
    // Both are `CLight` too (`lights.cpp:234`). 4 `light_directional` and no
    // `light_glspot` in the shipped maps; they cost a line each and the
    // alternative is a map that spawns an entity the game would not.
    ClassDef {
        name: "light_directional",
        keys: light::LIGHT_KEYS,
        inputs: LIGHT_INPUTS,
        outputs: &[],
        create: Light::create,
    },
    ClassDef {
        name: "light_glspot",
        keys: light::LIGHT_KEYS,
        inputs: LIGHT_INPUTS,
        outputs: &[],
        create: Light::create,
    },
    ClassDef {
        name: "light_environment",
        keys: light::ENV_LIGHT_KEYS,
        inputs: LIGHT_INPUTS,
        outputs: &[],
        create: EnvLight::create,
    },
    ClassDef {
        name: "func_brush",
        keys: brush::BRUSH_KEYS,
        inputs: BRUSH_INPUTS,
        outputs: &[],
        create: Brush::create,
    },
    // The two areaportal classnames share one behaviour: `vbsp` has already
    // taken both brushes away, and what is left of each is a `portalnumber`
    // and three inputs. See [`AreaPortal`](brush::AreaPortal) for what the
    // window half does not do.
    ClassDef {
        name: "func_areaportal",
        keys: brush::AREAPORTAL_KEYS,
        inputs: brush::AREAPORTAL_INPUTS,
        outputs: &[],
        create: brush::AreaPortal::create,
    },
    ClassDef {
        name: "func_areaportalwindow",
        keys: brush::AREAPORTAL_KEYS,
        inputs: brush::AREAPORTAL_INPUTS,
        outputs: &[],
        create: brush::AreaPortal::create,
    },
    ClassDef {
        name: "func_instance_io_proxy",
        keys: &[],
        inputs: PROXY_INPUTS,
        outputs: logic::PROXY_RELAYS,
        create: InstanceIoProxy::create,
    },
    // `CRotDoor : public CBaseDoor` — the same struct, told at construction
    // that it turns instead of sliding. It outnumbers `func_door`, which is
    // why `portdocs/SERVER.md` §4.7 says to write `AngularMove` first.
    ClassDef {
        name: "func_door_rotating",
        keys: brush::DOOR_KEYS,
        inputs: DOOR_INPUTS,
        outputs: brush::DOOR_OUTPUTS,
        create: Door::create_rotating,
    },
    ClassDef {
        name: "func_door",
        keys: brush::DOOR_KEYS,
        inputs: DOOR_INPUTS,
        outputs: brush::DOOR_OUTPUTS,
        create: Door::create,
    },
    ClassDef {
        name: "func_movelinear",
        keys: brush::MOVELINEAR_KEYS,
        inputs: MOVELINEAR_INPUTS,
        outputs: &["OnFullyOpen", "OnFullyClosed"],
        create: MoveLinear::create,
    },
    ClassDef {
        name: "func_button",
        keys: brush::BUTTON_KEYS,
        inputs: BUTTON_INPUTS,
        outputs: brush::BUTTON_OUTPUTS,
        create: Button::create,
    },
    ClassDef {
        name: "func_rotating",
        keys: brush::ROTATING_KEYS,
        inputs: ROTATING_INPUTS,
        outputs: brush::ROTATING_OUTPUTS,
        create: Rotating::create,
    },
    ClassDef {
        name: "logic_auto",
        keys: &["globalstate"],
        inputs: &[],
        outputs: &[
            "OnMapSpawn",
            "OnNewGame",
            "OnLoadGame",
            "OnMapTransition",
            "OnBackgroundMap",
            "OnMultiNewMap",
            "OnMultiNewRound",
        ],
        create: Auto::create,
    },
    ClassDef {
        name: "logic_branch",
        keys: &["InitialValue"],
        inputs: BRANCH_INPUTS,
        outputs: &["OnTrue", "OnFalse"],
        create: Branch::create,
    },
    ClassDef {
        name: "logic_branch_listener",
        keys: BRANCH_LIST_KEYS,
        inputs: BRANCH_LIST_INPUTS,
        outputs: &["OnAllTrue", "OnAllFalse", "OnMixed"],
        create: BranchList::create,
    },
    ClassDef {
        name: "logic_timer",
        keys: &[
            "StartDisabled",
            "RefireTime",
            "UseRandomTime",
            "LowerRandomBound",
            "UpperRandomBound",
        ],
        inputs: TIMER_INPUTS,
        outputs: &["OnTimer", "OnTimerHigh", "OnTimerLow"],
        create: Timer::create,
    },
    ClassDef {
        name: "math_counter",
        keys: &["min", "max", "startvalue", "StartDisabled"],
        inputs: MATH_COUNTER_INPUTS,
        outputs: &[
            "OutValue",
            "OnHitMin",
            "OnHitMax",
            "OnChangedFromMin",
            "OnChangedFromMax",
            "OnGetValue",
        ],
        create: MathCounter::create,
    },
    ClassDef {
        name: "env_tonemap_controller",
        // Measured: it has none of its own in any shipped map, and its
        // datadesc declares none either — every setting is an input.
        keys: &[],
        inputs: TONEMAP_INPUTS,
        outputs: &[],
        create: TonemapController::create,
    },
    ClassDef {
        name: "logic_case",
        keys: CASE_KEYS,
        inputs: CASE_INPUTS,
        outputs: CASE_OUTPUTS,
        create: Case::create,
    },
    // Stage 4: the trigger family, in census order.
    ClassDef {
        name: "trigger_once",
        keys: trigger::ONCE_KEYS,
        inputs: TRIGGER_INPUTS,
        outputs: trigger::MULTIPLE_OUTPUTS,
        create: TriggerMultiple::create_once,
    },
    ClassDef {
        name: "trigger_multiple",
        keys: trigger::MULTIPLE_KEYS,
        inputs: TRIGGER_INPUTS,
        outputs: trigger::MULTIPLE_OUTPUTS,
        create: TriggerMultiple::create,
    },
    ClassDef {
        name: "trigger_hurt",
        keys: trigger::HURT_KEYS,
        inputs: HURT_INPUTS,
        outputs: trigger::HURT_OUTPUTS,
        create: TriggerHurt::create,
    },
    ClassDef {
        name: "trigger_push",
        keys: trigger::PUSH_KEYS,
        inputs: PUSH_INPUTS,
        outputs: trigger::BASE_TRIGGER_OUTPUTS,
        create: TriggerPush::create,
    },
    ClassDef {
        name: "trigger_teleport",
        keys: trigger::TELEPORT_KEYS,
        inputs: TELEPORT_INPUTS,
        outputs: trigger::BASE_TRIGGER_OUTPUTS,
        create: TriggerTeleport::create,
    },
    // …the filters they consult…
    ClassDef {
        name: "filter_activator_class",
        keys: filter::FILTER_CLASS_KEYS,
        inputs: FILTER_INPUTS,
        outputs: filter::FILTER_OUTPUTS,
        create: FilterClass::create,
    },
    ClassDef {
        name: "filter_activator_name",
        keys: filter::FILTER_NAME_KEYS,
        inputs: FILTER_INPUTS,
        outputs: filter::FILTER_OUTPUTS,
        create: FilterName::create,
    },
    ClassDef {
        name: "filter_multi",
        keys: filter::FILTER_MULTI_KEYS,
        inputs: FILTER_INPUTS,
        outputs: filter::FILTER_OUTPUTS,
        create: FilterMulti::create,
    },
    ClassDef {
        name: "filter_player_held",
        keys: filter::BASE_FILTER_KEYS,
        inputs: FILTER_INPUTS,
        outputs: filter::FILTER_OUTPUTS,
        create: FilterPlayerHeld::create,
    },
    ClassDef {
        name: "filter_damage_type",
        keys: filter::FILTER_DAMAGE_TYPE_KEYS,
        inputs: FILTER_INPUTS,
        outputs: filter::FILTER_OUTPUTS,
        create: FilterDamageType::create,
    },
    ClassDef {
        name: "filter_activator_model",
        keys: filter::FILTER_MODEL_KEYS,
        inputs: FILTER_INPUTS,
        outputs: filter::FILTER_OUTPUTS,
        create: FilterModel::create,
    },
    // …and the two entities that move somebody.
    ClassDef {
        name: "point_teleport",
        keys: &[],
        inputs: POINT_TELEPORT_INPUTS,
        outputs: &[],
        create: PointTeleport::create,
    },
    // `LINK_ENTITY_TO_CLASS( player, CPortal_Player )`. **No shipped map
    // places one** — the player is created when a client connects, which here
    // is `Server::spawn_player` — but the classname is registered because
    // Valve registers it, because `filter_activator_class` compares against
    // it, and because a map is allowed to.
    ClassDef {
        name: "player",
        keys: player::PLAYER_KEYS,
        inputs: player::PLAYER_INPUTS,
        outputs: player::PLAYER_OUTPUTS,
        create: Player::create,
    },
    // `portdocs/SERVER.md` stage 5's two map-facing player classes.
    // `logic_playerproxy` is the player's *interface* to the map — 9 in the
    // game, and every one of the five output connections in the whole game is
    // on `sp_a1_intro1`. `player_loadsaved` is the game's other way of dying:
    // 9 entities, 11 `Reload` connections, mostly named `fade_to_death`.
    ClassDef {
        name: "logic_playerproxy",
        keys: player::PLAYER_PROXY_KEYS,
        inputs: player::PLAYER_PROXY_INPUTS,
        outputs: player::PLAYER_PROXY_OUTPUTS,
        create: LogicPlayerProxy::create,
    },
    ClassDef {
        name: "player_loadsaved",
        keys: player::REVERT_SAVED_KEYS,
        inputs: player::REVERT_SAVED_INPUTS,
        outputs: player::REVERT_SAVED_OUTPUTS,
        create: RevertSaved::create,
    },
    ClassDef {
        name: "info_target",
        keys: &[],
        inputs: &[],
        outputs: &[],
        create: PointEntity::create,
    },
    // Portal 2's own, and the first class here from `game/server/portal2/`.
    // 65 across 47 maps, one of them on `sp_a1_intro1`.
    ClassDef {
        name: "prop_floor_button",
        keys: prop::FLOOR_BUTTON_KEYS,
        inputs: FLOOR_BUTTON_INPUTS,
        outputs: prop::FLOOR_BUTTON_OUTPUTS,
        create: FloorButton::create,
    },
    // …and the second, out of order so that the two sit together. 138 across
    // 71 maps, **two of them on `sp_a1_intro1`** — and despite the classname
    // it is a `CBaseAnimating` rather than a prop, so it shares nothing with
    // its neighbours here but the file it is written in.
    ClassDef {
        name: "prop_testchamber_door",
        keys: prop::TESTCHAMBER_DOOR_KEYS,
        inputs: TESTCHAMBER_DOOR_INPUTS,
        outputs: prop::TESTCHAMBER_DOOR_OUTPUTS,
        create: TestChamberDoor::create,
    },
    // …and the third. **98 across 59 maps, one of them on `sp_a1_intro1`**,
    // and the first class here that is a `CPhysicsProp` — which this port has
    // no physics for, so what it is is a model, a skin ladder and nine inputs.
    // See [`WeightedCube`] for the accounting of what that leaves out.
    ClassDef {
        name: "prop_weighted_cube",
        keys: prop::WEIGHTED_CUBE_KEYS,
        inputs: WEIGHTED_CUBE_INPUTS,
        outputs: prop::WEIGHTED_CUBE_OUTPUTS,
        create: WeightedCube::create,
    },
    // …and the fourth, which is `portdocs/PORTAL.md` stage 2. 21 across 10
    // maps, **two of them on `sp_a1_intro1`**. It draws no model — see
    // [`PropPortal::model_state`] — and what you see where one is, is
    // `engine::world::portals`' overlay quad.
    ClassDef {
        name: "prop_portal",
        keys: portal::PORTAL_KEYS,
        inputs: PORTAL_INPUTS,
        outputs: portal::PORTAL_OUTPUTS,
        create: PropPortal::create,
    },
    // Never in an entity lump: one is created by every `prop_floor_button`'s
    // `Spawn`. It is in the table because the table is the *factory*, and
    // `Context::create_entity` goes through it.
    ClassDef {
        name: "trigger_portal_button",
        keys: BUTTON_TRIGGER_KEYS,
        inputs: TRIGGER_INPUTS,
        outputs: trigger::BASE_TRIGGER_OUTPUTS,
        create: ButtonTrigger::create,
    },
    ClassDef {
        name: "info_player_start",
        keys: &[],
        inputs: &[],
        outputs: &[],
        create: PointEntity::create,
    },
    ClassDef {
        name: "worldspawn",
        keys: world::WORLD_KEYS,
        inputs: &[],
        outputs: &[],
        create: World::create,
    },
];

// The input tables. They are `static`s rather than inline array literals
// because a `ClassDef` holds `&'static [InputDef]` and several classes share
// one.

static RELAY_INPUTS: InputDefs = &[
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("EnableRefire", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("Trigger", FieldType::Void),
    InputDef::new("CancelPending", FieldType::Void),
];

static BRANCH_INPUTS: InputDefs = &[
    InputDef::new("SetValue", FieldType::Bool),
    InputDef::new("SetValueTest", FieldType::Bool),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("ToggleTest", FieldType::Void),
    InputDef::new("Test", FieldType::Void),
];

/// `Branch01`…`Branch16`. Sixteen slots; the game uses ten.
static BRANCH_LIST_KEYS: &[&str] = &[
    "Branch01", "Branch02", "Branch03", "Branch04", "Branch05", "Branch06", "Branch07", "Branch08",
    "Branch09", "Branch10", "Branch11", "Branch12", "Branch13", "Branch14", "Branch15", "Branch16",
];

/// All three are `FIELD_INPUT` in the datadesc — the value is passed through
/// unconverted — and all three are fired with `Variant::Void` by everything
/// that fires them. The two underscored ones are a [`Branch`] talking to its
/// listeners and are not mapper-facing.
static BRANCH_LIST_INPUTS: InputDefs = &[
    InputDef::new("Test", FieldType::Input),
    InputDef::new(logic::INPUT_BRANCH_CHANGED, FieldType::Input),
    InputDef::new(logic::INPUT_BRANCH_REMOVED, FieldType::Input),
];

static TIMER_INPUTS: InputDefs = &[
    InputDef::new("RefireTime", FieldType::Float),
    InputDef::new("FireTimer", FieldType::Void),
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
    InputDef::new("Toggle", FieldType::Void),
    InputDef::new("AddToTimer", FieldType::Float),
    InputDef::new("ResetTimer", FieldType::Void),
    InputDef::new("SubtractFromTimer", FieldType::Float),
    // The three `DEFINE_INPUT`s: both a map key and a run-time input, with no
    // handler function in the C++ at all — `AcceptInput` writes the field
    // directly (`baseentity.cpp:4569`). They are the reason
    // `ClassDef::inputs` and `ClassDef::keys` can name the same string.
    InputDef::new("UseRandomTime", FieldType::Int),
    InputDef::new("LowerRandomBound", FieldType::Float),
    InputDef::new("UpperRandomBound", FieldType::Float),
];

static MATH_COUNTER_INPUTS: InputDefs = &[
    InputDef::new("Add", FieldType::Float),
    InputDef::new("Divide", FieldType::Float),
    InputDef::new("Multiply", FieldType::Float),
    InputDef::new("SetValue", FieldType::Float),
    InputDef::new("SetValueNoFire", FieldType::Float),
    InputDef::new("SetMaxValueNoFire", FieldType::Float),
    InputDef::new("SetMinValueNoFire", FieldType::Float),
    InputDef::new("Subtract", FieldType::Float),
    InputDef::new("SetHitMax", FieldType::Float),
    InputDef::new("SetHitMin", FieldType::Float),
    InputDef::new("GetValue", FieldType::Void),
    InputDef::new("Enable", FieldType::Void),
    InputDef::new("Disable", FieldType::Void),
];

static CASE_INPUTS: InputDefs = &[
    // `FIELD_INPUT`: the value arrives unconverted, which is what lets
    // `logic_case` compare its string form against the case keys whatever
    // type it came as.
    InputDef::new("InValue", FieldType::Input),
    InputDef::new("PickRandom", FieldType::Void),
    InputDef::new("PickRandomShuffle", FieldType::Void),
];

// The tables the class files own, under the names [`CLASSES`] reads.
static TRIGGER_INPUTS: InputDefs = trigger::BASE_TRIGGER_INPUTS;
static FLOOR_BUTTON_INPUTS: InputDefs = prop::FLOOR_BUTTON_INPUTS;
static DYNAMIC_PROP_INPUTS: InputDefs = prop::DYNAMIC_PROP_INPUTS;
static TESTCHAMBER_DOOR_INPUTS: InputDefs = prop::TESTCHAMBER_DOOR_INPUTS;
static WEIGHTED_CUBE_INPUTS: InputDefs = prop::WEIGHTED_CUBE_INPUTS;
static PORTAL_INPUTS: InputDefs = portal::PORTAL_INPUTS;
/// `CBaseTrigger`'s two keys, which a `trigger_portal_button` is never offered
/// — it is built from code — but which its `key_value` forwards, so they are
/// declared. The invariant test checks the declaration against the code, not
/// against the map data.
static BUTTON_TRIGGER_KEYS: &[&str] = &["StartDisabled", "filtername"];
static HURT_INPUTS: InputDefs = trigger::HURT_INPUTS;
static PUSH_INPUTS: InputDefs = trigger::PUSH_INPUTS;
static TELEPORT_INPUTS: InputDefs = trigger::TELEPORT_INPUTS;
static FILTER_INPUTS: InputDefs = filter::FILTER_INPUTS;
static POINT_TELEPORT_INPUTS: InputDefs = point::POINT_TELEPORT_INPUTS;
static BRUSH_INPUTS: InputDefs = brush::BRUSH_INPUTS;
static DOOR_INPUTS: InputDefs = brush::DOOR_INPUTS;
static MOVELINEAR_INPUTS: InputDefs = brush::MOVELINEAR_INPUTS;
static BUTTON_INPUTS: InputDefs = brush::BUTTON_INPUTS;
static ROTATING_INPUTS: InputDefs = brush::ROTATING_INPUTS;
static LIGHT_INPUTS: InputDefs = light::LIGHT_INPUTS;
static PROXY_INPUTS: InputDefs = logic::PROXY_INPUTS;
static TONEMAP_INPUTS: InputDefs = env::TONEMAP_INPUTS;
static CASE_KEYS: &[&str] = logic::CASE_KEYS;
static CASE_OUTPUTS: &[&str] = logic::CASE_OUTPUTS;

/// `EntityFactoryDictionary()->Create( className )`'s lookup half.
///
/// Case-insensitive, because `CUtlDict` is. Every classname in every shipped
/// map is already lower case, so this costs nothing and removes a class of
/// bug that would only appear on someone else's map.
pub fn lookup(classname: &str) -> Option<&'static ClassDef> {
    CLASSES
        .iter()
        .find(|class| class.name.eq_ignore_ascii_case(classname))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::class::{base_input, SpawnResult, BASE_INPUTS};
    use crate::server::entity::Entity;
    use crate::server::keyvalue;
    use crate::server::test_support::Harness;
    use glam::Vec3;

    #[test]
    fn lookup_is_case_insensitive_and_knows_nothing_it_was_not_given() {
        assert!(lookup("logic_relay").is_some());
        assert!(lookup("LOGIC_RELAY").is_some());
        assert!(lookup("ambient_generic").is_none());
        assert_eq!(lookup("light_spot").map(|c| c.name), Some("light_spot"));
    }

    /// No class may declare a name twice, and no two classes may share one.
    #[test]
    fn the_class_table_is_well_formed() {
        let mut names: Vec<&str> = CLASSES.iter().map(|c| c.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate classname in the table");

        for class in CLASSES {
            let mut keys: Vec<String> = class.keys.iter().map(|k| k.to_ascii_lowercase()).collect();
            keys.sort();
            let before = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), before, "{}: duplicate key", class.name);

            let mut outputs: Vec<String> = class
                .outputs
                .iter()
                .map(|o| o.to_ascii_lowercase())
                .collect();
            outputs.sort();
            let before = outputs.len();
            outputs.dedup();
            assert_eq!(outputs.len(), before, "{}: duplicate output", class.name);

            let mut inputs: Vec<String> = class
                .inputs
                .iter()
                .map(|i| i.name.to_ascii_lowercase())
                .collect();
            inputs.sort();
            let before = inputs.len();
            inputs.dedup();
            assert_eq!(inputs.len(), before, "{}: duplicate input", class.name);
        }
    }

    /// The declaration and the implementation must agree: every name in
    /// [`ClassDef::keys`] has to be a name `key_value` actually takes, and no
    /// class may consume a key it did not declare. Without this the unhandled
    /// report drifts from the truth in whichever direction the typo went.
    #[test]
    fn every_declared_key_is_consumed_and_every_consumed_key_is_declared() {
        for class in CLASSES {
            for key in class.keys {
                let mut entity = Entity::new(class);
                let Entity { core, behaviour } = &mut entity;
                assert!(
                    behaviour.key_value(core, key, "1"),
                    "{}: declares {key} but key_value refuses it",
                    class.name
                );
            }
            // A key nothing declares must be refused, including one another
            // class in the table owns.
            for other in CLASSES {
                for key in other.keys {
                    if class.declares_key(key) {
                        continue;
                    }
                    let mut entity = Entity::new(class);
                    let Entity { core, behaviour } = &mut entity;
                    assert!(
                        !behaviour.key_value(core, key, "1"),
                        "{}: consumes {key} without declaring it",
                        class.name
                    );
                }
            }
        }
    }

    /// The same invariant for inputs, and it is the one with teeth in stage 2:
    /// an input a class declares but does not handle is counted as *unhandled*
    /// at run time, so the depot test's totals would silently absorb the typo.
    #[test]
    fn every_declared_input_is_handled_and_every_handled_input_is_declared() {
        let mut harness = Harness::new();
        for class in CLASSES {
            for input in class.inputs {
                assert!(
                    harness.offer_input(class, input.name, input.field),
                    "{}: declares input {} but accept_input refuses it",
                    class.name,
                    input.name
                );
            }
            for other in CLASSES {
                for input in other.inputs {
                    if class.input_type(input.name).is_some() {
                        continue;
                    }
                    assert!(
                        !harness.offer_input(class, input.name, input.field),
                        "{}: handles input {} without declaring it",
                        class.name,
                        input.name
                    );
                }
            }
        }
    }

    /// A class may not shadow a `CBaseEntity` input by accident. `Toggle` is
    /// not a base input — three classes declare their own — but `Kill` and
    /// `Use` are, and a class quietly claiming one would change what `Kill`
    /// means for that class alone.
    #[test]
    fn no_class_shadows_a_base_input() {
        for class in CLASSES {
            for input in class.inputs {
                assert!(
                    base_input(input.name).is_none(),
                    "{} declares {}, which is also a CBaseEntity input",
                    class.name,
                    input.name
                );
            }
        }
        // …and the base table itself is well formed.
        let mut names: Vec<String> = BASE_INPUTS
            .iter()
            .map(|i| i.name.to_ascii_lowercase())
            .collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    /// `CLight::Spawn`'s one rule, which removes 6,937 of the shipped game's
    /// 7,150 lights.
    #[test]
    fn an_unnamed_light_removes_itself_and_a_named_one_does_not() {
        let mut harness = Harness::new();
        let class = lookup("light").expect("registered");

        let mut entity = Entity::new(class);
        assert_eq!(harness.spawn(&mut entity), SpawnResult::Remove);

        let mut entity = Entity::new(class);
        keyvalue::base_key_value(&mut entity.core, "targetname", "flicker");
        assert_eq!(harness.spawn(&mut entity), SpawnResult::Ok);
    }

    /// `pitch` writes `angles.x` and leaves the rest, and it is the contained
    /// [`Light`] that does it for a `light_environment` — the composition that
    /// replaced `BaseClass::KeyValue`.
    #[test]
    fn env_light_inherits_the_light_keys_by_holding_one() {
        let mut entity = Entity::new(lookup("light_environment").expect("registered"));
        keyvalue::base_key_value(&mut entity.core, "angles", "0 90 0");
        let Entity { core, behaviour } = &mut entity;

        assert!(behaviour.key_value(core, "pitch", "-45"));
        assert_eq!(core.angles, Vec3::new(-45.0, 90.0, 0.0));
        assert!(behaviour.key_value(core, "style", "33"));
        assert!(behaviour.key_value(core, "_light", "255 200 150 400"));
        assert!(behaviour.key_value(core, "_ambient", "1 2 3 4"), "consumed");
        assert!(!behaviour.key_value(core, "_lightHDR", "1 1 1 1"), "vrad's");

        let env = behaviour
            .downcast_ref::<EnvLight>()
            .expect("light_environment is an EnvLight");
        assert_eq!(env.sun_color, [255, 200, 150, 400u32 as u8]);
        assert_eq!(env.light().style, 33);
    }

    #[test]
    fn the_proxy_declares_thirty_distinct_relays() {
        let class = lookup("func_instance_io_proxy").expect("registered");
        assert_eq!(class.outputs.len(), 30);
        assert_eq!(class.inputs.len(), 30, "the inputs share the names");
        assert!(class.declared_output("onproxyrelay16").is_some());
        assert!(class.declared_output("OnProxyRelay30").is_some());
        // The unnumbered one the FGD shows a mapper is not an output the
        // server has ever had, and the 71 connections that fire an unnumbered
        // `ProxyRelay` input reach nothing either.
        assert!(class.declared_output("OnProxyRelay").is_none());
        assert!(class.input_type("ProxyRelay").is_none());
    }
}
