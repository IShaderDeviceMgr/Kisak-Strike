//! Cvars: the objects, their value semantics, and the registry.
//!
//! Replaces `tier1/convar.cpp`'s `ConVar` (the value half) and
//! `vstdlib/cvar.cpp`'s `CCvar` (the registry half). `portdocs/ENGINE_CONSOLE.md`
//! §6.1 and §6.2 are the design; the short version is that **the registry is not
//! the shared thing, the value is**.
//!
//! # Why there is no global
//!
//! `ENGINE.md` §7.4 originally called the cvar registry "the one piece of
//! ambient global state that is genuinely process-global". `ENGINE_CONSOLE.md`
//! §6.1 reverses that, and this module is the reversal: [`Console::cvar`]
//! hands back a [`Cvar`], which is an `Arc` around a cell of atomics. A
//! subsystem that wants `mat_luxels` keeps *the cvar*, not a way to look one
//! up, so reading it is an atomic load through the holder's own handle — no
//! lock, no hash probe, no `&Console` in the reader's signature, and callable
//! from any thread.
//!
//! [`CvarRegistry`] therefore serves exactly one caller: the dispatcher,
//! resolving a name that someone typed. That is why it is a plain `HashMap`
//! and why nothing here is lazily initialized.
//!
//! [`Console::cvar`]: super::Console::cvar
//!
//! # What that deletes
//!
//! `FCVAR_MATERIAL_SYSTEM_THREAD`/`FCVAR_ACCESSIBLE_FROM_THREADS` and the
//! `CCvar::QueueMaterialThreadSetValue` deferred-write queue behind them
//! (`vstdlib/cvar.cpp:774`) exist so that a cvar read off the material thread
//! could be written from the main one. An atomic cell makes the problem not
//! exist, so `ENGINE_CONSOLE.md` §4.6 deletes all three.
//!
//! The larger deletion is `RegisterConCommand`'s duplicate handling
//! (`vstdlib/cvar.cpp:361-450`), which linked a second `ConVar` of the same
//! name as a *child* of the first so that `sv_cheats`, declared separately in
//! `engine`, `client.so` and `server.so`, resolved to one value. One binary,
//! one declaration: a duplicate is a bug here and [`CvarRegistry::register`]
//! refuses it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

/// What a cvar's flags can say.
///
/// **The bit values are ours.** `ENGINE_CONSOLE.md` §4.6: no shipped content
/// spells a flag numerically, so only the *meanings* are fixed. These are
/// packed densely from zero rather than reproducing `public/tier1/iconvar.h`'s
/// numbering, which would leave twenty-two holes for flags this port does not
/// have.
///
/// Only the flags §4.6 marks "Keep" are here. The untrusted-source set
/// (`REPLICATED`, `SERVER_CAN_EXECUTE`, `USERINFO`, …) is deliberately absent
/// rather than present-and-ignored: they are a security model (§4.7), and a
/// flag that exists but is never checked reads as though it were.
///
/// Deliberately **not** shared with [`CommandFlags`]. In the original, bit 10
/// is `FCVAR_PRINTABLEONLY` on a `ConVar` and `FCVAR_GAMEDLL_FOR_REMOTE_CLIENTS`
/// on a `ConCommand` — the same bit meaning two things depending on what holds
/// it. Two types make that collision unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CvarFlags(u32);

impl CvarFlags {
    pub const NONE: CvarFlags = CvarFlags(0);
    /// `FCVAR_DEVELOPMENTONLY`. Hidden from listings, and **not settable** —
    /// `CCvarUtilities::IsCommand` (`engine/cvar.cpp:391`) bails before the set
    /// path, so the name reads as unknown.
    pub const DEVELOPMENTONLY: CvarFlags = CvarFlags(1 << 0);
    /// `FCVAR_HIDDEN`. Hidden from listings but still settable.
    pub const HIDDEN: CvarFlags = CvarFlags(1 << 1);
    /// `FCVAR_ARCHIVE`. Written to `config.cfg`. This flag *is* what that file
    /// is; stage 3 of `ENGINE_CONSOLE.md` §8 is its consumer.
    pub const ARCHIVE: CvarFlags = CvarFlags(1 << 2);
    /// `FCVAR_NEVER_AS_STRING`. The string form is not maintained on set, so
    /// [`Cvar::string`] keeps returning the initial text. Numeric readers are
    /// unaffected; listings format the number instead.
    pub const NEVER_AS_STRING: CvarFlags = CvarFlags(1 << 3);
    /// `FCVAR_CHEAT`. Settable only while `sv_cheats` is on — `CanCheat()`
    /// (`engine/gl_cvars.h:30`).
    pub const CHEAT: CvarFlags = CvarFlags(1 << 4);
    /// `FCVAR_SPONLY`. Single-player only. Everything is single-player until
    /// `server/` exists, so this is carried and never denies anything yet.
    pub const SPONLY: CvarFlags = CvarFlags(1 << 5);

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: CvarFlags) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Whether *any* of `other`'s bits are set. `RevertFlaggedConVars` wants
    /// this rather than `contains`.
    pub const fn intersects(self, other: CvarFlags) -> bool {
        (self.0 & other.0) != 0
    }
}

impl std::ops::BitOr for CvarFlags {
    type Output = CvarFlags;
    fn bitor(self, rhs: CvarFlags) -> CvarFlags {
        CvarFlags(self.0 | rhs.0)
    }
}

/// What a command's flags can say. See [`CvarFlags`] for why this is a
/// separate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandFlags(u32);

impl CommandFlags {
    pub const NONE: CommandFlags = CommandFlags(0);
    pub const DEVELOPMENTONLY: CommandFlags = CommandFlags(1 << 0);
    pub const HIDDEN: CommandFlags = CommandFlags(1 << 1);
    pub const CHEAT: CommandFlags = CommandFlags(1 << 2);
    pub const SPONLY: CommandFlags = CommandFlags(1 << 3);

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: CommandFlags) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Whether *any* of `other`'s bits are set. Completion wants this, to
    /// exclude `DEVELOPMENTONLY | HIDDEN` in one test.
    pub const fn intersects(self, other: CommandFlags) -> bool {
        (self.0 & other.0) != 0
    }
}

impl std::ops::BitOr for CommandFlags {
    type Output = CommandFlags;
    fn bitor(self, rhs: CommandFlags) -> CommandFlags {
        CommandFlags(self.0 | rhs.0)
    }
}

/// The storage behind a [`Cvar`].
///
/// Keeps Valve's **triple cache** (`tier1/convar.cpp:848-1065`): the string,
/// the float and the int are all recomputed on every set, so that `GetFloat`
/// in a hot loop is a load rather than a parse. That design is right and maps
/// onto atomics directly, which is the other half of why §6.1 works.
///
/// The three value fields are not updated as one transaction — a reader that
/// loaded `float` and then `string` across a concurrent set can see one from
/// either side of it. Valve has the same property (and no atomics at all), and
/// no consumer reads two representations together. What *is* ordered is
/// [`generation`](CvarCell::generation): it is stored with `Release` after the
/// values and loaded with `Acquire`, so a poller that observes a new
/// generation observes the value that came with it.
#[derive(Debug)]
pub struct CvarCell {
    name: Box<str>,
    help: Box<str>,
    default: Box<str>,
    flags: CvarFlags,
    /// `m_bHasMin`/`m_fMinVal` and `m_bHasMax`/`m_fMaxVal`, which Valve stores
    /// as a bool-plus-value pair per bound.
    min: Option<f32>,
    max: Option<f32>,
    /// `f32::to_bits`; there is no `AtomicF32`.
    float: AtomicU32,
    int: AtomicI32,
    string: RwLock<Arc<str>>,
    generation: AtomicU32,
}

/// A handle to one cvar's value.
///
/// Cheap to clone and safe to hold forever: the cell outlives the console if
/// the holder does, which is what the `Arc` is for.
///
/// **Hold a `Cvar`, never a `&CvarCell`.** `ENGINE_CONSOLE.md` §9 open question
/// 1 records this as the invariant that keeps the fallback design — a
/// console-owned registry with index handles — a mechanical change rather than
/// a rewrite of every caller.
#[derive(Debug, Clone)]
pub struct Cvar(Arc<CvarCell>);

impl Cvar {
    /// Builds a cvar that is not in any registry.
    ///
    /// For tests and for the handful of values that are cvar-shaped without
    /// being typeable. Prefer [`Console::cvar`](super::Console::cvar).
    pub fn detached(name: &str, default: &str, flags: CvarFlags, help: &str) -> Cvar {
        let cell = CvarCell {
            name: name.into(),
            help: help.into(),
            default: default.into(),
            flags,
            min: None,
            max: None,
            float: AtomicU32::new(0),
            int: AtomicI32::new(0),
            string: RwLock::new(Arc::from("")),
            generation: AtomicU32::new(0),
        };
        let cvar = Cvar(Arc::new(cell));
        // The initial value goes through the set path, so that the default is
        // clamped and the triple cache is coherent from the start --
        // `ConVar::Create` calls `InternalSetValue` for exactly this reason.
        cvar.set_string(default);
        cvar
    }

    /// [`detached`](Cvar::detached) with `ClampValue` bounds.
    pub fn detached_with_bounds(
        name: &str,
        default: &str,
        flags: CvarFlags,
        help: &str,
        min: Option<f32>,
        max: Option<f32>,
    ) -> Cvar {
        let mut cell = match Arc::try_unwrap(Cvar::detached(name, default, flags, help).0) {
            Ok(cell) => cell,
            Err(_) => unreachable!("the handle was just created and never cloned"),
        };
        cell.min = min;
        cell.max = max;
        let cvar = Cvar(Arc::new(cell));
        // Re-run the set now that the bounds exist: `ClampValue` applies to the
        // initial value too, so a default outside its own bounds is clamped
        // rather than being the one value that escapes them.
        cvar.set_string(default);
        cvar
    }

    pub fn name(&self) -> &str {
        &self.0.name
    }

    pub fn help(&self) -> &str {
        &self.0.help
    }

    /// The value the cvar was declared with, which [`revert`](Cvar::revert)
    /// restores.
    pub fn default_value(&self) -> &str {
        &self.0.default
    }

    pub fn flags(&self) -> CvarFlags {
        self.0.flags
    }

    pub fn bounds(&self) -> (Option<f32>, Option<f32>) {
        (self.0.min, self.0.max)
    }

    pub fn float(&self) -> f32 {
        f32::from_bits(self.0.float.load(Ordering::Relaxed))
    }

    pub fn int(&self) -> i32 {
        self.0.int.load(Ordering::Relaxed)
    }

    /// `ConVar::GetBool`, which is `GetInt() != 0`.
    pub fn bool(&self) -> bool {
        self.int() != 0
    }

    pub fn string(&self) -> Arc<str> {
        Arc::clone(&self.0.string.read().expect("cvar string lock poisoned"))
    }

    /// Bumped on every change. `ENGINE_CONSOLE.md` §6.2: this replaces
    /// `FnChangeCallback_t`, which cannot be stored here because a callback
    /// that needs `&mut` engine state cannot be owned by a registry the engine
    /// owns.
    ///
    /// Prefer [`changed`](Cvar::changed) over comparing this by hand.
    pub fn generation(&self) -> u32 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Whether the value changed since `last` was written, updating it.
    ///
    /// ```ignore
    /// if self.fps_max.changed(&mut self.fps_max_generation) {
    ///     clock.set_fps_max(self.fps_max.float());
    /// }
    /// ```
    ///
    /// Seeded from [`generation`](Cvar::generation) at construction, a poller
    /// reacts to every change after it started watching and not to the initial
    /// value; seeded from zero, it also fires once for the declared value,
    /// which is usually what a consumer that must apply the cvar wants.
    pub fn changed(&self, last: &mut u32) -> bool {
        let now = self.generation();
        let changed = now != *last;
        *last = now;
        changed
    }

    /// `ConVar::InternalSetValue` (`tier1/convar.cpp:848`).
    ///
    /// The clamp happens **before** the string is decided, and that ordering is
    /// visible: when `ClampValue` moves the value, the stored string becomes
    /// the reformatted number rather than the text that was typed, so
    /// `fps_max -1` against a minimum of 0 reads back as `"0.000000"` and not
    /// as `"-1"`. When the clamp does nothing, the string is kept exactly as
    /// typed, which is what lets a string cvar hold something non-numeric.
    pub fn set_string(&self, value: &str) {
        let parsed = atod(value);
        let mut float = parsed as f32;
        if !float.is_finite() {
            // `Warning( "Warning: %s = '%s' is infinite, clamping value.\n" )`.
            float = f32::MAX;
        }

        let clamped = self.clamp(&mut float);
        // Valve keeps the double for the int conversion so that a value beyond
        // f32's integer precision still truncates from the wider type.
        let wide = if clamped { float as f64 } else { parsed };

        let text: Option<Arc<str>> = if self.0.flags.contains(CvarFlags::NEVER_AS_STRING) {
            None
        } else if clamped {
            Some(Arc::from(format_float(float).as_str()))
        } else {
            Some(Arc::from(value))
        };

        self.store(float, wide as i32, text);
    }

    /// `ConVar::InternalSetFloatValue` (`tier1/convar.cpp:965`).
    ///
    /// Returns early when the value is unchanged, so the generation counter
    /// does not tick for a set that set nothing — a poller would otherwise wake
    /// every frame for a cvar written every frame with the same number.
    pub fn set_float(&self, value: f32) {
        if value == self.float() {
            return;
        }
        let mut value = value;
        self.clamp(&mut value);
        let text = (!self.0.flags.contains(CvarFlags::NEVER_AS_STRING))
            .then(|| Arc::from(format_float(value).as_str()));
        self.store(value, value as i32, text);
    }

    /// `ConVar::InternalSetIntValue` (`tier1/convar.cpp:1010`).
    pub fn set_int(&self, value: i32) {
        if value == self.int() {
            return;
        }
        let mut float = value as f32;
        let stored = if self.clamp(&mut float) {
            float as i32
        } else {
            value
        };
        let text = (!self.0.flags.contains(CvarFlags::NEVER_AS_STRING))
            .then(|| Arc::from(stored.to_string().as_str()));
        self.store(float, stored, text);
    }

    pub fn set_bool(&self, value: bool) {
        self.set_int(i32::from(value));
    }

    /// `ConVar::Revert`. What `sv_cheats 0` does to every `FCVAR_CHEAT` cvar,
    /// through [`CvarRegistry::revert_flagged`].
    pub fn revert(&self) {
        let default = self.0.default.clone();
        self.set_string(&default);
    }

    /// `ConVar::ClampValue` (`tier1/convar.cpp:945`). True when it moved.
    fn clamp(&self, value: &mut f32) -> bool {
        if let Some(min) = self.0.min {
            if *value < min {
                *value = min;
                return true;
            }
        }
        if let Some(max) = self.0.max {
            if *value > max {
                *value = max;
                return true;
            }
        }
        false
    }

    fn store(&self, float: f32, int: i32, text: Option<Arc<str>>) {
        self.0.float.store(float.to_bits(), Ordering::Relaxed);
        self.0.int.store(int, Ordering::Relaxed);
        if let Some(text) = text {
            *self.0.string.write().expect("cvar string lock poisoned") = text;
        }
        // Release, and last: everything above must be visible to a reader that
        // sees the new generation.
        self.0.generation.fetch_add(1, Ordering::Release);
    }
}

/// `printf("%f")`, which is what Valve reformats a clamped value with.
///
/// Six decimals is not cosmetic here — it is the text that reaches
/// `config.cfg`, and `exec` has to read back what the writer produced.
fn format_float(value: f32) -> String {
    format!("{value:.6}")
}

/// `V_atod`, i.e. C's `atof`: parse the longest numeric prefix and ignore the
/// rest, yielding zero when there is no prefix at all.
///
/// Rust's `str::parse` is strict, and strictness is wrong here: this reads
/// shipped `.cfg` files, where a trailing comment or a stray unit would
/// otherwise turn a real value into a parse failure. `ENGINE_CONSOLE.md` §7 —
/// the file format is Valve's, so its number grammar is too.
pub(super) fn atod(text: &str) -> f64 {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let mut end = 0;

    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
    }
    if end == digits_start || (end == digits_start + 1 && bytes[digits_start] == b'.') {
        // No digits on either side of the point: `atof` yields 0.
        return 0.0;
    }

    // An exponent only counts when it is complete; `atof("1e")` is 1.
    let mantissa_end = end;
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let mut exp = end + 1;
        if exp < bytes.len() && (bytes[exp] == b'+' || bytes[exp] == b'-') {
            exp += 1;
        }
        let exp_digits = exp;
        while exp < bytes.len() && bytes[exp].is_ascii_digit() {
            exp += 1;
        }
        if exp > exp_digits {
            end = exp;
        }
    }

    text[..end]
        .parse()
        .or_else(|_| text[..mantissa_end].parse())
        .unwrap_or(0.0)
}

/// Why a registration was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// `vstdlib/cvar.cpp:361` linked a same-named newcomer as a *child* of the
    /// incumbent so that one value was shared across three DLLs. There is one
    /// binary here, so a duplicate is a bug — `ENGINE_CONSOLE.md` §4.8.
    #[error("`{0}` is already registered")]
    Duplicate(String),
    /// A name that could never be typed, which would be a silent dead entry.
    #[error("`{0}` is not a usable name")]
    BadName(String),
}

/// Name to cvar.
///
/// The whole of what `vstdlib/cvar.cpp`'s `CCvar` survives as, minus the
/// `IAppSystem` lifecycle, the iterator factory, the DLL identifiers and the
/// parent/child linkage. `vstdlib/concommandhash.h`'s hand-rolled
/// open-addressing table is a `HashMap`.
///
/// Lookup is ASCII-case-insensitive, matching `FindVar`'s `Q_stricmp`; the
/// key is lowercased and the cell keeps the name as declared.
#[derive(Debug, Default)]
pub struct CvarRegistry {
    by_name: HashMap<Box<str>, Cvar>,
}

impl CvarRegistry {
    pub fn new() -> CvarRegistry {
        CvarRegistry::default()
    }

    pub fn register(&mut self, cvar: Cvar) -> Result<Cvar, RegisterError> {
        let key = cvar.name().to_ascii_lowercase();
        if key.is_empty() || key.split_whitespace().count() != 1 {
            return Err(RegisterError::BadName(cvar.name().to_string()));
        }
        if self.by_name.contains_key(key.as_str()) {
            return Err(RegisterError::Duplicate(cvar.name().to_string()));
        }
        self.by_name.insert(key.into_boxed_str(), cvar.clone());
        Ok(cvar)
    }

    pub fn find(&self, name: &str) -> Option<&Cvar> {
        self.by_name.get(name.to_ascii_lowercase().as_str())
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cvar> {
        self.by_name.values()
    }

    /// `CCvar::RevertFlaggedConVars` (`vstdlib/cvar.cpp`), which `sv_cheats 0`
    /// calls with `FCVAR_CHEAT`. Returns how many were reverted.
    pub fn revert_flagged(&self, flags: CvarFlags) -> usize {
        self.by_name
            .values()
            .filter(|cvar| cvar.flags().intersects(flags))
            .map(Cvar::revert)
            .count()
    }
}
