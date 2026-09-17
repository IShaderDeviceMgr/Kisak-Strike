//! What the game asks a studio model about its animation — and the whole of
//! it.
//!
//! `CBaseAnimating`'s `LookupSequence`, `SequenceDuration` and
//! `SequenceLoops` (`baseanimating.cpp:1060`, `:1214`, `:2270`), which in the
//! original reach a `CStudioHdr` through `modelinfo->GetModelPtr`. This module
//! names no studio type and never will: it holds the *answers*, copied in once
//! per level by whoever loaded the models.
//!
//! # Why it is a table and not a call
//!
//! `server/` names no `studio/` type, the same way it names no `wgpu` type and
//! no `world/` type — which is what keeps all of its tests runnable without a
//! window, a GPU or a map. The three questions above are the only ones any
//! class asks, their answers are two numbers and a bit, and a whole level's
//! worth is a few hundred entries. So the engine fills a [`SequenceTable`] in
//! once, at the same moment it uploads the models, and the server reads it
//! through [`Context::sequence`](super::class::Context::sequence).
//!
//! That is also the shape `world/` already has in the other direction: a
//! `Placement` is the *answer* to "where is this brush model", copied across
//! once a frame rather than called for.
//!
//! # A model nobody loaded is not a model with no sequences
//!
//! The two have to be told apart, which is why [`Lookup`] has three cases and
//! not two. The order a level loads in is
//!
//! ```text
//!     World::load  →  Server::level_init  →  World::load_entity_models
//! ```
//!
//! and it cannot be anything else: the models an entity places are named by
//! the entities, which do not exist until `level_init` has run. So **every
//! `Spawn` in the game runs against an empty table**, and a class that asked
//! "does my model have this sequence?" there would be told no for all 8,462
//! `prop_dynamic`s in the game.
//!
//! [`Lookup::Unknown`] is that case, and the rule a caller follows is: treat
//! it as Valve's `LookupSequence` succeeding — the map named a sequence and
//! there is nothing here to contradict it — and as `SequenceDuration`
//! returning nothing, so an animation whose model was never loaded never
//! finishes. Both are what an entity with no model on screen should do.

use std::collections::HashMap;

/// One sequence, reduced to what a game class reads.
///
/// `SequenceDuration` and `SequenceLoops`. Everything else `mstudioseqdesc_t`
/// carries — the activity, the events, the blend table, the pose parameters,
/// the IK rules — is the *renderer's* or belongs to a system this port has
/// not got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SequenceInfo {
    /// `Studio_Duration`: `(numframes - 1) / fps`, in seconds. **A one-frame
    /// sequence is zero**, which is why every caller guards the division.
    pub duration: f32,
    /// `GetSequenceFlags() & STUDIO_LOOPING`.
    pub loops: bool,
    /// `mstudioseqdesc_t::fadeouttime`, in **seconds** — 0.2 for 10,664 of
    /// the shipped game's 10,666 sequences and 0.5 for the other two.
    ///
    /// Here for one reader: `CBaseAnimating::GetLastVisibleCycle`, and so
    /// [`last_visible_cycle`](SequenceInfo::last_visible_cycle). It is *not*
    /// a blend time — nothing in this port cross-fades.
    pub fade_out_time: f32,
}

impl SequenceInfo {
    /// `CBaseAnimating::GetSequenceCycleRate` (`baseanimating.cpp:1028`) —
    /// sequence lengths per second at a playback rate of 1.
    ///
    /// **A zero-length sequence is `1/0.1`, not infinity**, which is Valve's
    /// guard and is why a one-frame sequence "finishes" in a tenth of a
    /// second rather than never.
    pub fn cycle_rate(&self) -> f32 {
        match self.duration > 0.0 {
            true => 1.0 / self.duration,
            false => 1.0 / 0.1,
        }
    }

    /// `CBaseAnimating::GetLastVisibleCycle` (`baseanimating.cpp:1043`) — the
    /// cycle past which `m_bSequenceFinished` is set.
    ///
    /// > **A non-looping sequence is "finished" `fade_out_time` seconds before
    /// > it ends**, because `fadeouttime * cycle_rate` is that many seconds
    /// > expressed in cycles. For the test chamber door's 0.9167-second `open`
    /// > that is cycle 0.782, i.e. 0.717 seconds in — which is when its
    /// > `OnFullyOpen` fires, not at 0.917.
    ///
    /// The `playback_rate` factor is Valve's and is signed, so **playing
    /// backwards puts the threshold above 1 and out of reach**: a reversed
    /// sequence can only be finished by running off the bottom, which is the
    /// `flNewCycle < 0` branch of `StudioFrameAdvanceInternal`.
    pub fn last_visible_cycle(&self, playback_rate: f32) -> f32 {
        match self.loops {
            true => 1.0,
            false => 1.0 - self.fade_out_time * self.cycle_rate() * playback_rate,
        }
    }
}

/// What [`SequenceTable::lookup`] found. See the module docs for why there are
/// three answers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lookup {
    /// The table has never heard of this model — nobody has loaded it yet, or
    /// its file would not read. **Not** "it has no such sequence".
    Unknown,
    /// The model is loaded and has no sequence by that name. This is Valve's
    /// `LookupSequence() == ACTIVITY_NOT_AVAILABLE`, and 183 `DefaultAnim`
    /// keys in the shipped game are it.
    Missing,
    Found(SequenceInfo),
}

/// Every sequence of every model the game's entities place, by model path and
/// label.
///
/// Both keys are compared **case-insensitively**, because `LookupSequence` is
/// `stricmp` and because map data spells model paths with either slash and
/// either case.
#[derive(Debug, Clone, Default)]
pub struct SequenceTable {
    models: HashMap<String, HashMap<String, SequenceInfo>>,
}

impl SequenceTable {
    pub fn new() -> SequenceTable {
        SequenceTable::default()
    }

    /// Records one model's whole sequence list. Replaces anything already
    /// filed under that path.
    pub fn insert_model(
        &mut self,
        model: &str,
        sequences: impl IntoIterator<Item = (String, SequenceInfo)>,
    ) {
        let labels = sequences
            .into_iter()
            .map(|(label, info)| (fold(&label), info))
            .collect();
        self.models.insert(fold(model), labels);
    }

    /// `LookupSequence` and the two questions that follow it, in one call.
    pub fn lookup(&self, model: &str, label: &str) -> Lookup {
        let Some(labels) = self.models.get(&fold(model)) else {
            return Lookup::Unknown;
        };
        match labels.get(&fold(label)) {
            Some(info) => Lookup::Found(*info),
            None => Lookup::Missing,
        }
    }

    /// How many models the table describes. For the startup log.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

/// The key form: lower case, and `\` as `/`.
///
/// `V_FixSlashes` plus `stricmp`. Map data writes `models\props\x.mdl` about
/// as often as it writes forward slashes, and `Vfs` already folds both — this
/// is the same rule one layer up, so that the table a model was filed under
/// matches the string the entity kept.
fn fold(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\\' => '/',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> SequenceTable {
        let mut t = SequenceTable::new();
        t.insert_model(
            "models/props/portal_button.mdl",
            [
                (
                    "down".to_owned(),
                    SequenceInfo {
                        duration: 0.4166,
                        loops: false,
                        fade_out_time: 0.2,
                    },
                ),
                (
                    "spin".to_owned(),
                    SequenceInfo {
                        duration: 2.0,
                        loops: true,
                        fade_out_time: 0.2,
                    },
                ),
            ],
        );
        t
    }

    #[test]
    fn a_model_nobody_loaded_is_not_a_model_without_the_sequence() {
        let t = table();
        assert_eq!(
            t.lookup("models/props/never_loaded.mdl", "down"),
            Lookup::Unknown
        );
        assert_eq!(
            t.lookup("models/props/portal_button.mdl", "sideways"),
            Lookup::Missing
        );
        assert!(matches!(
            t.lookup("models/props/portal_button.mdl", "down"),
            Lookup::Found(_)
        ));
    }

    /// Both halves of the key are `stricmp`, and a backslash is a slash.
    #[test]
    fn the_lookup_folds_case_and_slashes() {
        let t = table();
        assert!(matches!(
            t.lookup("MODELS\\Props\\Portal_Button.mdl", "DOWN"),
            Lookup::Found(_)
        ));
    }

    #[test]
    fn the_loop_flag_comes_back_with_the_duration() {
        let t = table();
        let Lookup::Found(spin) = t.lookup("models/props/portal_button.mdl", "spin") else {
            panic!("spin is in the table")
        };
        assert!(spin.loops);
        assert_eq!(spin.duration, 2.0);
    }
}
