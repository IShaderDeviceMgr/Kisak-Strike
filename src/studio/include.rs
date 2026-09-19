//! `$includemodel` — a model whose animation lives in a different `.mdl`.
//!
//! `CStudioHdr::ResolveIncludedModels` and the `virtualmodel_t` it builds
//! (`public/studio_virtualmodel.cpp`): a `.mdl` can name other `.mdl` files
//! whose sequences and animations it borrows, and the runtime merges them into
//! one flat list of sequences the model then behaves as though it owned.
//!
//! **It is what makes nine of Portal 2's models animate at all.** Those nine
//! are worn by 926 `prop_dynamic`s, 898 of them the `models/anim_wp/room_transform`
//! panel arms, and every animation label their maps name lives in a companion
//! `*_animation.mdl` rather than in the model the map places. Without this the
//! labels resolve to nothing, so the props stand in their bind pose and their
//! `AnimThink` cancels on the first tick.
//!
//! # What of the virtual model survives
//!
//! Valve keeps the included files *separate* and indirects through them for
//! ever: `virtualmodel_t` is a list of groups, each with a `masterSeq`,
//! `masterAnim`, `masterBone` and `boneMap` remapping table, and every
//! `pSeqdesc`/`pAnimdesc`/`CalcVirtualAnimation` call hops through one. It has
//! to, because the underlying `studiohdr_t`s are cache entries that can be
//! evicted independently.
//!
//! Nothing here can be evicted — a [`StudioModel`](super::StudioModel) is an
//! owned value — so the groups collapse: the remap is applied **once**, at
//! load, and what comes out is one bone list, one sequence list and one
//! animation list indistinguishable from a model that had them all along.
//! `masterSeq`, `boneMap`, `masterAttachment`, the pose/node/IK-lock tables
//! and the whole `CModelLookupContext` string-table optimisation go with it.
//!
//! What does not collapse is [`masterBone`](merge): an included animation's
//! `mstudio_rle_anim_t::bone` indexes the **included** model's bone list, and
//! the two skeletons are the same bones in a different order in eight of the
//! nine. Merging without remapping poses the wrong bone — a silently wrong
//! picture rather than an error.

use super::anim::Animation;
use super::mdl::Mdl;

/// How deep a chain of includes is followed.
///
/// `AppendModels` recurses with no depth limit at all and relies on content
/// being acyclic; this is a bound rather than a feature, and the measurement
/// under it is that **no shipped model's include includes anything** — all 26
/// `$includemodel` hosts in Portal 2 are one level deep. The recursion is here
/// because Valve's is, not because the data needs it.
const MAX_DEPTH: usize = 8;

/// Merges every `$includemodel` `host` names into it, and returns the ones
/// that were actually merged.
///
/// `read` is given a path relative to the game root and returns the file, or
/// `None` if there is no such model — which is `FindModel` returning null, and
/// is a real case: all three of `models/props_lab/bot_male.mdl`'s includes are
/// absent from the shipped game. A missing or unreadable include is skipped,
/// exactly as it is in the original, and the host keeps whatever it had.
///
/// Taking a closure rather than a `Vfs` is what
/// [`assemble`](super::assemble) does and for the same reason: this module
/// then needs no filesystem and its tests need no game.
pub(super) fn resolve(
    host: &mut Mdl,
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();

    // `AppendModels` walks the list depth first and in order: the host's own
    // sequences, then include 1 and everything *it* includes, then include 2.
    // A stack popped from the back reproduces that if each level is pushed
    // reversed.
    let mut pending: Vec<(String, usize)> = host
        .include_models
        .iter()
        .rev()
        .map(|name| (name.clone(), 1))
        .collect();

    while let Some((name, depth)) = pending.pop() {
        // Valve has no cycle check and would recurse for ever on one. Nothing
        // in the game is cyclic; this is here so that a downloaded map's
        // model cannot hang the loader.
        if name == host.path || merged.iter().any(|done| *done == name) {
            continue;
        }
        let Some(bytes) = read(&name) else {
            // `FindModel` returned null. Not an error: the model simply has
            // fewer sequences than its compiler thought it would.
            continue;
        };
        let included = match Mdl::parse(name.clone(), &bytes) {
            Ok(included) => included,
            Err(e) => {
                eprintln!("source-engine: studio: {}: $includemodel {e}", host.path);
                continue;
            }
        };

        // Taken before the merge, which consumes the rest of it: an included
        // model's animation tables are the biggest thing in this module —
        // `arm64x64_interior_animation.mdl` expands to about 7 MB — and they
        // are moved into the host rather than copied into it.
        let children = included.include_models.clone();

        merge(host, included);
        merged.push(name);

        if depth < MAX_DEPTH {
            for child in children.into_iter().rev() {
                pending.push((child, depth + 1));
            }
        }
    }

    merged
}

/// Appends one included model's animations and sequences to `host`.
///
/// `AppendBonemap` + `AppendAnimations` + `AppendSequences`
/// (`studio_virtualmodel.cpp:283`, `:222`, `:139`), with the indirection
/// resolved rather than recorded.
///
/// Three rules, and the first is the only one that can produce a wrong
/// *picture* rather than a missing animation:
///
/// 1. **A bone is matched by name, case insensitively**, and an included bone
///    the host does not have is dropped along with its track. `masterBone` is
///    not the identity for eight of the nine models this reaches — the bone
///    *lists* agree and their *order* does not — so an unremapped merge would
///    animate the wrong joints.
/// 2. **Animations are deduplicated by name and sequences by label, with the
///    host winning**, which is `AppendAnimations`/`AppendSequences`' `k ==
///    numCheck` test. A sequence that survives points at whatever the *name*
///    dedup decided, so an include's sequence can end up playing the host's
///    own animation — that is Valve's, and it is how a map that asks for a
///    label both files carry gets the host's.
/// 3. **The values are decoded against the included model's bones**, because
///    `CalcVirtualAnimation` passes `&pAnimbone[panim->bone]` — the animation
///    file's bone — to `CalcBoneQuaternion`. That happens for free here: the
///    include was parsed as its own file, so its `posscale`, `rotscale` and
///    bind-pose `rot` are already the ones its RLE stream was written against.
fn merge(host: &mut Mdl, included: Mdl) {
    // `AppendBonemap`. Valve keeps both directions; only this one is needed,
    // because nothing here looks a host bone up in the include.
    let master_bone: Vec<Option<usize>> = included
        .bones
        .iter()
        .map(|bone| {
            host.bones
                .iter()
                .position(|host_bone| host_bone.name.eq_ignore_ascii_case(&bone.name))
        })
        .collect();

    // `AppendAnimations`, and the index map it leaves behind: an included
    // sequence names an animation by its *local* index, and `iRelativeAnim`
    // is what turns that into a global one.
    //
    // > **The dedup window is fixed before the loop, not as it grows.** Valve
    // > captures `int numCheck = m_anim.Count()` and searches only `k <
    // > numCheck`, so two animations with the *same name inside one group*
    // > are both appended — only a name an *earlier* group already had is
    // > folded away. Searching the whole list instead would quietly merge
    // > them, which would change which animation an included sequence plays.
    // > No shipped companion has a duplicate name, so this is fidelity rather
    // > than a fix.
    let known_animations = host.animations.len();
    let mut master_anim: Vec<usize> = Vec::with_capacity(included.animations.len());
    for animation in included.animations {
        let existing = host.animations[..known_animations]
            .iter()
            .position(|held| held.name.eq_ignore_ascii_case(&animation.name));
        master_anim.push(match existing {
            Some(index) => index,
            None => {
                host.animations.push(remap(animation, &master_bone));
                host.animations.len() - 1
            }
        });
    }

    // `AppendSequences`. The host is group 0 and wins every collision, which
    // is why `arm64x64_interior`'s one local `BindPose` survives its
    // include's 1,350.
    //
    // The one branch not ported is the `STUDIO_OVERRIDE` replacement — a
    // forward-declared, empty sequence that a later group is allowed to fill
    // in. **Not one of the game's 7,885 sequences sets the flag**, so it is
    // measured out rather than deferred.
    //
    // The window is fixed here too, and for the same reason as above.
    let known_sequences = host.sequences.len();
    for mut sequence in included.sequences {
        if host.sequences[..known_sequences]
            .iter()
            .any(|held| held.label.eq_ignore_ascii_case(&sequence.label))
        {
            continue;
        }
        sequence.anim = match master_anim.get(sequence.anim) {
            Some(&anim) => anim,
            // Only reachable for an include with no animations at all, since
            // `parse_sequences` has already clamped the index into its own
            // file's list. Point it past the end rather than at animation
            // zero: `StudioModel::animation` then answers `None`, which is
            // the bind pose, where a clamp would play something arbitrary.
            None => host.animations.len(),
        };
        host.sequences.push(sequence);
    }

    // `AppendAttachments` (`studio_virtualmodel.cpp:374`), with the same
    // fixed dedup window and the same host-wins rule as the two above — and
    // one rule of its own: **an attachment whose bone the host does not have
    // is dropped**, which is Valve's `if (n == -1) continue`.
    //
    // > **No shipped content reaches this.** Of the game's 1,952 attachment
    // > points, *none* arrives through a `$includemodel` — the 25 hosts carry
    // > their own and their companions carry animation and nothing else. So
    // > this is the reference's shape rather than live content, kept because
    // > the bone remap below is the same silent-wrong-answer trap the
    // > animation tracks have and because a companion that did carry one
    // > would otherwise ride the wrong bone.
    //
    // The bone index is remapped here rather than at lookup. Valve leaves it
    // pointing into the included file and remaps in
    // `CStudioHdr::GetAttachmentBone`, which is the same indirection the
    // groups exist for and the same one that collapses here — see the module
    // docs. Skip it and an attachment rides whichever of the host's bones
    // happens to share an index with the include's, which on eight of the
    // nine shipped hosts is a *different bone*.
    let known_attachments = host.attachments.len();
    for mut attachment in included.attachments {
        let Some(bone) = master_bone.get(attachment.bone).copied().flatten() else {
            continue;
        };
        if host.attachments[..known_attachments]
            .iter()
            .any(|held| held.name.eq_ignore_ascii_case(&attachment.name))
        {
            continue;
        }
        attachment.bone = bone;
        host.attachments.push(attachment);
    }
}

/// One animation, with every track moved onto the host's bone indices.
///
/// A track whose bone the host does not have is **dropped** — `masterBone` is
/// `-1` and `CalcVirtualAnimation`'s `if ( j >= 0 && … )` skips it. One bone in
/// the shipped game is in this case (`thigh_A_R_GRP`, in
/// `models/eggbot_animations.mdl` against `eggbot.mdl`'s 119).
fn remap(mut animation: Animation, master_bone: &[Option<usize>]) -> Animation {
    animation.tracks.retain_mut(
        |track| match master_bone.get(track.bone).copied().flatten() {
            Some(bone) => {
                track.bone = bone;
                true
            }
            None => false,
        },
    );
    animation
}

#[cfg(test)]
mod tests {
    use super::super::anim::{Animation, Attachment, Bone, BoneTrack, Sequence};
    use super::*;
    use glam::{Mat4, Quat, Vec3};

    fn bone(name: &str) -> Bone {
        Bone {
            name: name.to_owned(),
            parent: None,
            pos: Vec3::ZERO,
            quat: Quat::IDENTITY,
            rot: Vec3::ZERO,
            pos_scale: Vec3::ONE,
            rot_scale: Vec3::ONE,
            flags: 0,
            pose_to_bone: Mat4::IDENTITY,
        }
    }

    fn animation(name: &str, tracks: &[usize]) -> Animation {
        Animation {
            name: name.to_owned(),
            fps: 24.0,
            flags: 0,
            frame_count: 2,
            tracks: tracks
                .iter()
                .map(|&bone| BoneTrack {
                    bone,
                    pos: vec![Vec3::ZERO],
                    rot: vec![Quat::IDENTITY],
                })
                .collect(),
        }
    }

    fn sequence(label: &str, anim: usize) -> Sequence {
        Sequence {
            label: label.to_owned(),
            flags: 0,
            fade_out_time: 0.2,
            bounds: (Vec3::ZERO, Vec3::ZERO),
            anim,
        }
    }

    /// An `Mdl` with nothing in it but a skeleton and an animation list — the
    /// only parts of one that a merge touches.
    fn model(path: &str, bones: &[&str]) -> Mdl {
        Mdl {
            path: path.to_owned(),
            name: path.to_owned(),
            checksum: 0,
            flags: Default::default(),
            bounds: (Vec3::ZERO, Vec3::ZERO),
            hull: (Vec3::ZERO, Vec3::ZERO),
            illum_position: Vec3::ZERO,
            bone_count: bones.len() as u32,
            bones: bones.iter().map(|name| bone(name)).collect(),
            sequences: Vec::new(),
            animations: Vec::new(),
            attachments: Vec::new(),
            include_models: Vec::new(),
            textures: Vec::new(),
            texture_dirs: Vec::new(),
            body_parts: Vec::new(),
        }
    }

    /// The whole point of the module: an included animation's bone indices are
    /// the *include's*, and the two skeletons are the same bones in a
    /// different order.
    ///
    /// Without the remap this is the one failure that draws something rather
    /// than nothing — the arm bends at the wrong joint.
    #[test]
    fn a_tracks_bone_is_remapped_by_name_and_not_by_index() {
        let mut host = model("host.mdl", &["root", "arm", "tip"]);
        let mut included = model("anim.mdl", &["tip", "root", "arm"]);
        included.animations = vec![animation("wave", &[0, 2])];
        included.sequences = vec![sequence("wave", 0)];

        merge(&mut host, included);

        let tracks: Vec<usize> = host.animations[0].tracks.iter().map(|t| t.bone).collect();
        // Include bone 0 is `tip`, which is host bone 2; include bone 2 is
        // `arm`, which is host bone 1. Read as indices they would have stayed
        // 0 and 2.
        assert_eq!(tracks, vec![2, 1]);
        assert_eq!(host.sequences.len(), 1);
        assert_eq!(host.sequences[0].label, "wave");
        assert_eq!(host.sequences[0].anim, 0);
    }

    fn attachment(name: &str, bone: usize) -> Attachment {
        Attachment {
            name: name.to_owned(),
            flags: 0,
            bone,
            local: Mat4::IDENTITY,
        }
    }

    /// `AppendAttachments` (`studio_virtualmodel.cpp:374`), and the same trap
    /// the animation tracks have: an included attachment's `localbone` indexes
    /// the **include's** bone list.
    ///
    /// Without the remap the attachment rides whichever host bone happens to
    /// share its index — a wrong place that still looks like a place, which is
    /// the failure mode a child parented to it would inherit.
    #[test]
    fn an_included_attachments_bone_is_remapped_by_name_and_not_by_index() {
        let mut host = model("host.mdl", &["root", "arm", "tip"]);
        let mut included = model("anim.mdl", &["tip", "root", "arm"]);
        // On include bone 0, which is `tip` — host bone 2.
        included.attachments = vec![attachment("muzzle", 0)];

        merge(&mut host, included);

        assert_eq!(host.attachments.len(), 1);
        assert_eq!(host.attachments[0].name, "muzzle");
        assert_eq!(host.attachments[0].bone, 2, "read as an index it stays 0");
    }

    /// The host is group 0 and wins every name collision, exactly as it does
    /// for sequences and animations — and an attachment whose bone the host
    /// does not have is dropped, which is Valve's `if (n == -1) continue`.
    #[test]
    fn the_host_keeps_its_own_attachment_and_drops_one_with_no_bone() {
        let mut host = model("host.mdl", &["root"]);
        host.attachments = vec![attachment("muzzle", 0)];
        let mut included = model("anim.mdl", &["root", "tail"]);
        included.attachments = vec![
            // Same name as the host's: dropped, host wins.
            attachment("MUZZLE", 1),
            // On `tail`, which the host does not have: dropped.
            attachment("tip", 1),
            // Kept.
            attachment("base", 0),
        ];

        merge(&mut host, included);

        let names: Vec<&str> = host.attachments.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["muzzle", "base"]);
        assert_eq!(host.attachments[0].bone, 0);
    }

    /// `masterBone` of -1: the host has no such bone, so the track goes.
    #[test]
    fn a_track_for_a_bone_the_host_does_not_have_is_dropped() {
        let mut host = model("host.mdl", &["root"]);
        let mut included = model("anim.mdl", &["root", "tail"]);
        included.animations = vec![animation("wag", &[0, 1])];
        included.sequences = vec![sequence("wag", 0)];

        merge(&mut host, included);

        let tracks: Vec<usize> = host.animations[0].tracks.iter().map(|t| t.bone).collect();
        assert_eq!(tracks, vec![0], "the `tail` track should have been dropped");
    }

    /// Bones are matched with `stricmp`, like every other name in the format.
    #[test]
    fn bones_sequences_and_animations_all_match_case_insensitively() {
        let mut host = model("host.mdl", &["Root", "ARM"]);
        host.animations = vec![animation("Idle", &[0])];
        host.sequences = vec![sequence("Idle", 0)];

        let mut included = model("anim.mdl", &["arm", "root"]);
        included.animations = vec![animation("IDLE", &[0]), animation("wave", &[0])];
        included.sequences = vec![sequence("idle", 0), sequence("Wave", 1)];

        merge(&mut host, included);

        // `IDLE` deduplicated onto the host's `Idle`, so only `wave` is new…
        assert_eq!(host.animations.len(), 2);
        assert_eq!(host.animations[1].name, "wave");
        // …and the host's own `Idle` sequence survived the include's `idle`.
        assert_eq!(host.sequences.len(), 2);
        assert_eq!(host.sequences[0].label, "Idle");
        assert_eq!(host.sequences[1].label, "Wave");
        assert_eq!(host.sequences[1].anim, 1, "`Wave` should play `wave`");
    }

    /// The dedup is by *name*, so an include's sequence can end up playing the
    /// **host's** animation — and then it is not remapped, because it never
    /// belonged to the include. This is `AppendAnimations`' `k == numCheck`
    /// and it is the subtle half of the merge.
    #[test]
    fn a_deduplicated_animation_keeps_the_hosts_own_tracks() {
        let mut host = model("host.mdl", &["root", "arm"]);
        host.animations = vec![animation("idle", &[1])];

        let mut included = model("anim.mdl", &["arm", "root"]);
        // The include's `idle` names *its* bone 0, which is the host's bone 1
        // — the same bone, by luck. What must survive is the host's track.
        included.animations = vec![animation("idle", &[0])];
        included.sequences = vec![sequence("stand", 0)];

        merge(&mut host, included);

        assert_eq!(host.animations.len(), 1, "no second `idle`");
        assert_eq!(host.sequences[0].label, "stand");
        assert_eq!(host.sequences[0].anim, 0, "pointed at the host's `idle`");
    }

    /// `AppendAnimations` captures `numCheck` **before** the loop, so a name
    /// repeated *within one include* is appended twice — only a collision with
    /// something an earlier group already had is folded away.
    ///
    /// Searching the whole list as it grows is the obvious implementation and
    /// is wrong: it would merge the two, and every sequence naming the second
    /// would silently play the first. No shipped companion has a duplicate
    /// name, so nothing in Portal 2 exercises this — it is here because the
    /// difference is invisible until some model does.
    #[test]
    fn the_dedup_window_is_fixed_before_the_include_rather_than_growing() {
        let mut host = model("host.mdl", &["root"]);
        host.animations = vec![animation("idle", &[0])];
        host.sequences = vec![sequence("idle", 0)];

        let mut included = model("anim.mdl", &["root"]);
        // Two of each name: the first collides with the host's, the second
        // collides only with its own sibling.
        included.animations = vec![
            animation("idle", &[0]),
            animation("walk", &[0]),
            animation("walk", &[0]),
        ];
        included.sequences = vec![sequence("walk", 1), sequence("walk", 2)];

        merge(&mut host, included);

        // The host's `idle` absorbed the include's; both `walk`s survived.
        assert_eq!(
            host.animations
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["idle", "walk", "walk"]
        );
        // Likewise both `walk` sequences, each on its own animation.
        assert_eq!(host.sequences.len(), 3);
        assert_eq!(host.sequences[1].anim, 1);
        assert_eq!(host.sequences[2].anim, 2);
    }

    /// `resolve` walks the list, skips what it cannot read, and reports what
    /// it merged.
    #[test]
    fn a_missing_include_is_skipped_and_not_an_error() {
        let mut host = model("host.mdl", &["root"]);
        host.include_models = vec!["gone.mdl".to_owned()];

        let merged = resolve(&mut host, |_| None);

        assert!(merged.is_empty());
        assert!(host.sequences.is_empty());
    }

    /// A model that includes itself, directly or round a ring, terminates.
    /// Valve's `AppendModels` would not.
    #[test]
    fn a_cycle_terminates() {
        let mut host = model("host.mdl", &["root"]);
        host.include_models = vec!["host.mdl".to_owned()];

        let merged = resolve(&mut host, |_| panic!("should not read the host itself"));

        assert!(merged.is_empty());
    }
}
