//! Turning the entity lump's strings into entity state.
//!
//! `CBaseEntity::ParseMapData`/`KeyValue` (`game/shared/baseentity_shared.cpp:334`
//! and `:361`) and the value grammar under them: `atoi`, `atof`,
//! `UTIL_StringToFloatArray` (`util_shared.cpp:1079`) and `V_StringToIntArray`
//! (`tier1/strtools.cpp:2959`).
//!
//! # Why the C runtime's parsing rules are ported rather than `str::parse`
//!
//! `atoi` and `atof` read a *prefix* and return 0 when there is nothing to
//! read; `"1.5abc"` is 1.5 and `""` is 0. `str::parse` rejects both. Map data
//! is written by a level editor and by `vbsp`, and the shipped maps contain
//! keys no `parse` would accept — a `logic_relay` with a key literally named
//! `//OnTrigger`, for one. Using `parse` here would turn Valve's "read what
//! you can" into a different value, silently, on real maps. So [`atoi`] and
//! [`atof`] reproduce the C semantics and [`string_to_vector`] reproduces the
//! zero fill.
//!
//! # Two splitters, and they are not the same
//!
//! `UTIL_StringToFloatArray` treats every byte `<= ' '` as a separator;
//! `V_StringToIntArray` splits on the space character alone. They are used for
//! vectors and for colours respectively, so a tab in a `rendercolor` behaves
//! differently from a tab in an `origin`. Both are reproduced. Measured across
//! the 106 shipped maps: **no `origin`, `angles` or `rendercolor` value
//! contains a tab, or a leading or trailing space**, so nothing in Portal 2
//! reaches the difference — but the next game's maps are not this one's.

use glam::Vec3;

use super::entity::EntityCore;

/// `EF_*` (`public/const.h:264`) — the render-flag bits the `KeyValue` ladder
/// sets. Only the ones some shipped map actually asks for; the rest of the
/// enum is set from code, not from map data.
pub mod effects {
    /// `EF_NOSHADOW` — 10,446 entities across the shipped maps.
    pub const NOSHADOW: u32 = 0x010;
    /// `EF_NORECEIVESHADOW` — 9,410.
    pub const NORECEIVESHADOW: u32 = 0x040;
    /// `EF_MARKED_FOR_FAST_REFLECTION` — 5,373.
    pub const MARKED_FOR_FAST_REFLECTION: u32 = 0x400;
    /// `EF_NOSHADOWDEPTH` — 5,023.
    pub const NOSHADOWDEPTH: u32 = 0x800;
    /// `EF_SHADOWDEPTH_NOCACHE` — 2,931 entities carry the key, and it is the
    /// one in the family that is tri-state: 1 sets, 2 *clears*, anything else
    /// does neither.
    pub const SHADOWDEPTH_NOCACHE: u32 = 0x1000;
    /// `EF_NOFLASHLIGHT` — 1,992.
    pub const NOFLASHLIGHT: u32 = 0x2000;
}

/// `EFL_NO_DAMAGE_FORCES` (`game/shared/baseentity_shared.h`) — the one place
/// the ladder sets an *entity* flag rather than an effect. 318 entities.
pub const EFL_NO_DAMAGE_FORCES: u32 = 0x0000_0001;

/// `CBaseEntity`'s own outputs — `DEFINE_OUTPUT( m_OnUser1, "OnUser1" )` and
/// its three siblings (`baseentity.cpp:2427`).
///
/// Every class has them, so they are checked here rather than repeated in
/// every [`ClassDef::outputs`](super::class::ClassDef::outputs). 604 `OnUser1`
/// connections across the shipped maps.
pub const BASE_OUTPUTS: &[&str] = &["OnUser1", "OnUser2", "OnUser3", "OnUser4"];

/// `atof`: read a leading float, or 0.
///
/// Accepts leading whitespace, a sign, digits with an optional point, and an
/// optional exponent; stops at the first byte that cannot continue the number.
pub fn atof(s: &str) -> f32 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits = false;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits = true;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits = true;
        }
    }
    if !digits {
        return 0.0;
    }
    // An exponent only counts if it has at least one digit — `strtod` backs
    // off to the mantissa otherwise, and so does this.
    if i < b.len() && (b[i] | 0x20) == b'e' {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exponent = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exponent {
            i = j;
        }
    }
    // Rust's float grammar accepts everything scanned above — `+5`, `5.`, `.5`
    // — and overflow saturates to infinity exactly as `strtod` does.
    s[start..i].parse::<f32>().unwrap_or(0.0)
}

/// `atoi`: read a leading integer, or 0.
///
/// Overflow is undefined in C and saturates here. No shipped map has a value
/// anywhere near the range, so this is a choice about what to do with
/// malformed data rather than a behavioural difference.
pub fn atoi(s: &str) -> i32 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits {
        return 0;
    }
    match s[start..i].parse::<i64>() {
        Ok(v) => v.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        // More than 19 digits. Saturate by sign rather than return 0.
        Err(_) => match b[start] == b'-' {
            true => i32::MIN,
            false => i32::MAX,
        },
    }
}

/// `UTIL_StringToFloatArray` (`util_shared.cpp:1079`).
///
/// Missing trailing components are **zeroed**, so `"5"` is `(5, 0, 0)` rather
/// than an error. Separators are any byte `<= ' '`.
///
/// Valve copies into a 128-byte buffer first, so a longer value is truncated.
/// Not reproduced: the longest `origin` in the shipped maps is 26 bytes, and
/// silently losing data is not a behaviour worth keeping.
pub fn string_to_float_array(s: &str, out: &mut [f32]) {
    let mut tokens = s
        .split(|c: char| (c as u32) <= 0x20)
        .filter(|t| !t.is_empty());
    for slot in out.iter_mut() {
        *slot = match tokens.next() {
            Some(token) => atof(token),
            None => 0.0,
        };
    }
}

/// `UTIL_StringToVector`.
pub fn string_to_vector(s: &str) -> Vec3 {
    let mut v = [0.0f32; 3];
    string_to_float_array(s, &mut v);
    Vec3::from(v)
}

/// `V_StringToIntArray` (`tier1/strtools.cpp:2959`). Returns how many
/// components the string supplied.
///
/// **Splits on the space character only**, unlike [`string_to_float_array`],
/// and the returned count is Valve's `j + 1` — so a string with more
/// components than `out` reports `out.len() + 1`, not `out.len()`. That is the
/// arithmetic [`string_to_color32`] depends on, so it is reproduced rather
/// than tidied.
pub fn string_to_int_array(s: &str, out: &mut [i32]) -> usize {
    let mut rest = s;
    let mut j = 0;
    while j < out.len() {
        out[j] = atoi(rest);
        match rest.find(' ') {
            Some(space) => rest = &rest[space + 1..],
            None => break,
        }
        j += 1;
    }
    let found = j + 1;
    for slot in out.iter_mut().skip(j + 1) {
        *slot = 0;
    }
    found
}

/// `V_StringToColor32` (`tier1/strtools.cpp:3031`).
///
/// Three components leave alpha at 255; four set it. Components are assigned
/// through `unsigned char`, so 300 is 44 — reproduced, because a map with an
/// out-of-range colour should look the way it looks in the shipped game.
pub fn string_to_color32(s: &str) -> [u8; 4] {
    let mut v = [0i32; 4];
    let found = string_to_int_array(s, &mut v);
    [
        v[0] as u8,
        v[1] as u8,
        v[2] as u8,
        match found == 4 {
            true => v[3] as u8,
            false => 255,
        },
    ]
}

/// `CBaseEntity::KeyValue` — the part every entity shares.
///
/// Returns whether the key was consumed. Run *before* the class's own
/// [`Behaviour::key_value`](super::class::Behaviour::key_value), which is the
/// order the original runs them in: the if-ladder first, then the datadesc
/// walk, with a derived class's override calling `BaseClass::KeyValue` last.
///
/// # What is deliberately not here
///
/// Four entries of Valve's ladder are omitted, and each is a measurement over
/// the 106 shipped maps rather than a judgement:
///
/// - **`angle`** (0 occurrences) — the legacy single-float yaw. Its
///   implementation is also **infinitely recursive**: it rewrites the value
///   and re-enters `KeyValue( szKeyName, szBuf )` with `szKeyName` still
///   `"angle"` (`baseentity_shared.cpp:475`), so every call re-parses and
///   recurses. Nothing in Portal 2 reaches it, which is presumably how it
///   survived.
/// - **`rendercolor32`** (0) — an alias for `rendercolor`.
/// - **`mins`/`maxs`** (0 each) — they set collision bounds, which belong to a
///   collision property this port has not got.
///
/// A `CBaseEntity` key whose *field* this port has not needed yet — `health`,
/// `effects`, `velocity` and two dozen more — is left unconsumed on purpose
/// rather than parsed into nothing, so that it appears in the unhandled report
/// instead of being quietly dropped the way the original drops it. `speed`
/// left that list at stage 3, when four mover classes started reading it.
pub fn base_key_value(entity: &mut EntityCore, key: &str, value: &str) -> bool {
    // Case-insensitive throughout: every name comparison in the original is
    // `FStrEq`, which is `stricmp`.
    let is = |name: &str| key.eq_ignore_ascii_case(name);

    // Consumed by `MapEntity_ParseEntity` before the entity exists — it is
    // what chose the class. Taken here so that it is not reported as a key
    // nobody understood.
    if is("classname") {
        return true;
    }

    if is("rendercolor") {
        entity.render_color = string_to_color32(value);
        return true;
    }
    if is("renderamt") {
        // `SetRenderAlpha`. Overwrites whatever `rendercolor` left, which is
        // why the two share a field and why lump order is observable.
        entity.render_color[3] = atoi(value) as u8;
        return true;
    }
    if is("rendermode") {
        entity.render_mode = atoi(value) as u8;
        return true;
    }
    if is("renderfx") {
        entity.render_fx = atoi(value) as u8;
        return true;
    }

    // The render-flag family. All of them are "non-zero sets the bit", except
    // `shadowdepthnocache`, which is tri-state.
    for (name, bit) in [
        ("disableshadows", effects::NOSHADOW),
        ("disablereceiveshadows", effects::NORECEIVESHADOW),
        ("drawinfastreflection", effects::MARKED_FOR_FAST_REFLECTION),
        ("disableshadowdepth", effects::NOSHADOWDEPTH),
        ("disableflashlight", effects::NOFLASHLIGHT),
    ] {
        if is(name) {
            if atoi(value) != 0 {
                entity.effects |= bit;
            }
            return true;
        }
    }
    if is("shadowdepthnocache") {
        match atoi(value) {
            1 => entity.effects |= effects::SHADOWDEPTH_NOCACHE,
            2 => entity.effects &= !effects::SHADOWDEPTH_NOCACHE,
            _ => {}
        }
        return true;
    }
    if is("nodamageforces") {
        if atoi(value) != 0 {
            entity.entity_flags |= EFL_NO_DAMAGE_FORCES;
        }
        return true;
    }

    if is("angles") {
        entity.angles = string_to_vector(value);
        return true;
    }
    if is("origin") {
        entity.origin = string_to_vector(value);
        return true;
    }
    if is("targetname") {
        entity.name = Some(value.to_owned());
        return true;
    }
    // `DEFINE_KEYFIELD( m_target, FIELD_STRING, "target" )`
    // (`baseentity.cpp:2217`) — a `CBaseEntity` field, which is why it is here
    // and not on the two classes that read it. Measured: `trigger_teleport`
    // (73) and `point_teleport` (128) are the only implemented classnames in
    // the whole game that carry the key.
    if is("target") {
        entity.target = Some(value.to_owned());
        return true;
    }

    // The datadesc half, for the fields [`EntityCore`] has.
    if is("parentname") {
        entity.parent_name = Some(value.to_owned());
        return true;
    }
    if is("model") {
        entity.model = Some(value.to_owned());
        return true;
    }
    if is("spawnflags") {
        entity.spawn_flags = atoi(value) as u32;
        return true;
    }
    // `DEFINE_KEYFIELD( m_flSpeed, FIELD_FLOAT, "speed" )`
    // (`baseentity.cpp:2219`). A `CBaseEntity` field, which is why it is here
    // rather than on the four mover classes that read it — and
    // `func_rotating` treats it as its *current* rotation rate rather than as
    // a setting, so it is written from code as often as from the map.
    if is("speed") {
        entity.speed = atof(value);
        return true;
    }
    if is("hammerid") {
        entity.hammer_id = u32::try_from(atoi(value)).ok();
        return true;
    }

    // The damage block — `CBaseEntity`'s, at `baseentity.cpp:2260`. All three
    // arrive with `portdocs/SERVER.md` stage 5.
    //
    // > **`health` is the 682-entity key the depot test used to count as
    // > unhandled**, and every one of them writes `0`. Consuming it here does
    // > not make a door shootable — a class has to set `m_takedamage` for
    // > that, and `CBaseDoor::Spawn` only does so above zero — it makes the
    // > key *read*, which is the difference between "not implemented" and
    // > "implemented and the maps ask for nothing".
    if is("health") {
        entity.health = atoi(value);
        return true;
    }
    if is("max_health") {
        entity.max_health = atoi(value);
        return true;
    }
    if is("damagefilter") {
        entity.damage_filter_name = Some(value.to_owned());
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::classes;
    use crate::server::entity::Entity;

    /// The prefix rule, which is the whole reason these exist.
    #[test]
    fn atof_and_atoi_read_a_prefix_and_never_fail() {
        assert_eq!(atof("1.5abc"), 1.5);
        assert_eq!(atof(""), 0.0);
        assert_eq!(atof("   -3  "), -3.0);
        assert_eq!(atof("+2.5e2"), 250.0);
        // An exponent with no digits is not an exponent.
        assert_eq!(atof("5e"), 5.0);
        assert_eq!(atof("5e+"), 5.0);
        assert_eq!(atof(".5"), 0.5);
        assert_eq!(atof("5."), 5.0);
        assert_eq!(atof("."), 0.0);
        assert_eq!(atof("OnTrigger"), 0.0);

        assert_eq!(atoi("255 255 255"), 255);
        assert_eq!(atoi("-7x"), -7);
        assert_eq!(atoi(""), 0);
        assert_eq!(atoi("  +12"), 12);
        assert_eq!(atoi("1.9"), 1, "atoi stops at the point");
    }

    /// `UTIL_StringToFloatArray`'s zero fill: a short value is not an error.
    #[test]
    fn a_short_vector_is_zero_filled() {
        assert_eq!(string_to_vector("1 2 3"), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(string_to_vector("5"), Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(string_to_vector(""), Vec3::ZERO);
        assert_eq!(
            string_to_vector("1 2 3 4"),
            Vec3::new(1.0, 2.0, 3.0),
            "extra components are dropped"
        );
        // Separators are anything <= ' ', which is what `UTIL_` uses.
        assert_eq!(string_to_vector("1\t2\n3"), Vec3::new(1.0, 2.0, 3.0));
    }

    /// `V_StringToColor32`'s alpha default is decided by a count that is
    /// `j + 1`, not by the number of components — see [`string_to_int_array`].
    #[test]
    fn a_three_component_colour_is_opaque_and_a_four_component_one_is_not() {
        assert_eq!(string_to_color32("255 128 0"), [255, 128, 0, 255]);
        assert_eq!(string_to_color32("1 2 3 4"), [1, 2, 3, 4]);
        // Assigned through `unsigned char`.
        assert_eq!(string_to_color32("300 0 0"), [44, 0, 0, 255]);
        // Five components overshoot the count test and fall back to opaque.
        assert_eq!(string_to_color32("1 2 3 4 5"), [1, 2, 3, 255]);
        // And a trailing space supplies a fourth component of zero, which is
        // transparent. No shipped Portal 2 map does this; reproduced anyway.
        assert_eq!(string_to_color32("1 2 3 "), [1, 2, 3, 0]);
    }

    fn core() -> EntityCore {
        Entity::new(classes::lookup("info_target").expect("info_target is registered")).core
    }

    #[test]
    fn the_shared_ladder_consumes_the_keys_every_entity_has() {
        let mut e = core();
        assert!(base_key_value(&mut e, "origin", "16 -32 64"));
        assert!(base_key_value(&mut e, "ANGLES", "0 90 0"));
        assert!(base_key_value(&mut e, "targetname", "relay_1"));
        assert!(base_key_value(&mut e, "spawnflags", "3"));
        assert!(base_key_value(&mut e, "hammerid", "172549"));
        assert!(base_key_value(&mut e, "model", "*3"));
        assert!(base_key_value(&mut e, "parentname", "platform"));

        assert_eq!(e.origin, Vec3::new(16.0, -32.0, 64.0));
        assert_eq!(e.angles, Vec3::new(0.0, 90.0, 0.0));
        assert_eq!(e.name.as_deref(), Some("relay_1"));
        assert_eq!(e.spawn_flags, 3);
        assert_eq!(e.hammer_id, Some(172_549));
        assert_eq!(e.model.as_deref(), Some("*3"));
        assert_eq!(e.parent_name.as_deref(), Some("platform"));

        assert!(!base_key_value(&mut e, "_quadratic_attn", "1"), "vrad's");
    }

    /// `rendercolor` fills alpha and `renderamt` then overwrites it, so the
    /// order the two appear in the lump is observable. It is the order the
    /// shipped maps use.
    #[test]
    fn renderamt_overwrites_the_alpha_rendercolor_set() {
        let mut e = core();
        base_key_value(&mut e, "rendercolor", "255 255 255");
        assert_eq!(e.render_color, [255, 255, 255, 255]);
        base_key_value(&mut e, "renderamt", "128");
        assert_eq!(e.render_color, [255, 255, 255, 128]);
    }

    #[test]
    fn the_render_flag_family_sets_effect_bits_and_one_of_them_clears() {
        let mut e = core();
        base_key_value(&mut e, "disableshadows", "1");
        base_key_value(&mut e, "disablereceiveshadows", "0");
        assert_eq!(e.effects, effects::NOSHADOW);

        base_key_value(&mut e, "shadowdepthnocache", "1");
        assert!(e.effects & effects::SHADOWDEPTH_NOCACHE != 0);
        base_key_value(&mut e, "shadowdepthnocache", "2");
        assert!(e.effects & effects::SHADOWDEPTH_NOCACHE == 0, "2 clears");
        base_key_value(&mut e, "shadowdepthnocache", "7");
        assert!(
            e.effects & effects::SHADOWDEPTH_NOCACHE == 0,
            "7 does nothing"
        );
    }
}
