//! The entity classes, and the table that turns a classname into one.
//!
//! `EntityFactoryDictionary` (`game/server/entitylist.cpp:60`) and the
//! `LINK_ENTITY_TO_CLASS`/`BEGIN_DATADESC` pairs scattered through
//! `game/server/`.
//!
//! # What is here, and what it covers
//!
//! Sixteen classnames, **17,479 of the shipped game's 60,925 entities**. Stage
//! 1 brought ten of them — `worldspawn`, the light family, `info_target`,
//! `info_player_start`, `logic_relay` and `func_instance_io_proxy` — as
//! keyvalue bags with one piece of behaviour between them. Stage 2 gives those
//! ten their behaviour and adds six more: `logic_auto`, `logic_branch`,
//! `logic_case`, `logic_timer`, `math_counter` and
//! `env_tonemap_controller`.
//!
//! The additions are not chosen by instance count — `logic_case` is 84
//! entities — but by what a map needs in order to *run*: `logic_auto` is how
//! every map in the game starts itself, and `env_tonemap_controller` is how
//! 105 of the 106 say how bright they should be.
//!
//! # One file per family
//!
//! `portdocs/SERVER.md` §7.1 lays out a `classes/` directory with a file per
//! family, and stage 1 said it would split "when stage 2's logic family
//! arrives". It has.

pub mod env;
pub mod light;
pub mod logic;
pub mod world;

use crate::server::class::{ClassDef, InputDef, InputDefs, PointEntity};
use crate::server::io::FieldType;

pub use env::TonemapController;
pub use light::{EnvLight, Light};
pub use logic::{Auto, Branch, Case, InstanceIoProxy, MathCounter, Relay, Timer};
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
        name: "func_instance_io_proxy",
        keys: &[],
        inputs: PROXY_INPUTS,
        outputs: logic::PROXY_RELAYS,
        create: InstanceIoProxy::create,
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
    ClassDef {
        name: "info_target",
        keys: &[],
        inputs: &[],
        outputs: &[],
        create: PointEntity::create,
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
        assert!(lookup("prop_dynamic").is_none());
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
