//! Entity I/O: the value type, the connections, and the queue that carries them.
//!
//! `game/server/cbase.cpp` (`CEventAction`, `CBaseEntityOutput`, `CEventQueue`,
//! `variant_t::Convert`), `game/server/entityoutput.h` and
//! `game/server/variant_t.h`.
//!
//! # This is the module, for this game
//!
//! `portdocs/SERVER.md` §0.4: 61,451 output connections across the 106 shipped
//! maps, on 17,091 entities. Nothing else in `game/server/` comes close to
//! mattering as much — the whole 122,298-line `ai_*` tree exists to serve 293
//! `npc_*` instances.
//!
//! # The shape, in one paragraph
//!
//! A map gives an entity a list of **connections**, one per output key
//! occurrence: *fire input `I` on everything named `T`, `D` seconds from now,
//! at most `N` more times*. Firing an output does **not** call anything — it
//! appends to one global [`EventQueue`], which is drained once a tick by
//! [`EventQueue::service`]. That indirection is the whole reason the Rust
//! borrow rules are not a problem here: an input handler that fires an output
//! that reaches its own entity is a queue append, not a re-entrant call, so
//! nothing is ever borrowed twice (`portdocs/SERVER.md` §10.3, which asked for
//! this to be checked early — it is, and the answer is that the C++ is not
//! re-entrant either).

use std::collections::BTreeMap;

use glam::Vec3;

use super::entity::EntityId;
use super::keyvalue::{atof, atoi, string_to_vector};

/// `EVENT_FIRE_ALWAYS` (`entityoutput.h:21`): a connection with no fire limit.
pub const EVENT_FIRE_ALWAYS: i32 = -1;

/// `VMF_IOPARAM_STRING_DELIMITER` (`public/entitydefs.h:17`) — an ESC.
///
/// Valve uses it in preference to a comma **so that a parameter may contain a
/// comma**, and falls back to a comma when the value has no ESC in it
/// (`cbase.cpp:128`).
///
/// Measured over the 106 shipped maps: **every one of the 61,451 connection
/// values uses the ESC and none uses a comma**, so the fallback is unreachable
/// in Portal 2. (`portdocs/SERVER.md` §4.3 records "61,375 ESC and 16 comma";
/// re-measuring over output-key values only finds no comma-delimited
/// connection at all — the sixteen are `AddOutput` *parameters*, which are
/// comma-separated by a different rule and are not connections.) The fallback
/// is reproduced regardless: it costs one line and a map from another game
/// reaches it.
const DELIMITER: char = '\u{1b}';

// ---------------------------------------------------------------------------
// variant_t
// ---------------------------------------------------------------------------

/// `variant_t` (`game/server/variant_t.h`) — the value an I/O connection
/// carries.
///
/// Valve's is a union plus a `fieldtype_t`; a Rust enum is the same thing with
/// the discriminant checked. The accessors are deliberately *not* the union
/// reads: `variant_t::Int()` returns 0 unless the field type is already
/// `FIELD_INTEGER`, so reading without converting first is how Valve's own
/// code gets zero — [`Variant::convert`] is what an input handler's declared
/// type runs first.
#[derive(Debug, Clone, PartialEq)]
pub enum Variant {
    /// `FIELD_VOID` — no value. What a parameterless output fires.
    Void,
    Bool(bool),
    Int(i32),
    Float(f32),
    String(String),
    Vector(Vec3),
    Color32([u8; 4]),
}

/// `fieldtype_t` (`public/datamap.h`), narrowed to what an input can declare.
///
/// Only the types some ported class actually declares, plus [`Input`](FieldType::Input),
/// which is Valve's "whatever came in".
///
/// Two of Valve's are deliberately absent, and each is a measurement rather
/// than an omission. **`FIELD_POSITION_VECTOR`**: `Convert` has no rule
/// producing one, and the two inputs that would want it (`SetLocalOrigin`,
/// `SetLocalAngles`) declare `FIELD_STRING` anyway. **`FIELD_EHANDLE`**: no
/// class in the port declares one, and both of its conversions need the entity
/// list — `FIELD_STRING` → `FIELD_EHANDLE` is `FindEntityByName`
/// (`cbase.cpp:1424`, whose comment says "by classname" and is wrong), and the
/// reverse takes the entity's targetname. Adding it means giving
/// [`Variant::convert`] a way to reach the list, and the first class that
/// declares an `FIELD_EHANDLE` input is the condition that makes that worth
/// doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Void,
    Bool,
    Int,
    Float,
    String,
    Vector,
    Color32,
    /// `FIELD_INPUT` — accepts the variant unchanged, whatever it is.
    Input,
}

impl Variant {
    /// The field type this value currently is. `variant_t::FieldType`.
    pub fn field_type(&self) -> FieldType {
        match self {
            Variant::Void => FieldType::Void,
            Variant::Bool(_) => FieldType::Bool,
            Variant::Int(_) => FieldType::Int,
            Variant::Float(_) => FieldType::Float,
            Variant::String(_) => FieldType::String,
            Variant::Vector(_) => FieldType::Vector,
            Variant::Color32(_) => FieldType::Color32,
        }
    }

    /// `variant_t::Convert` (`cbase.cpp:1289`) — the square table that decides
    /// which I/O types coerce into which.
    ///
    /// Returns whether the conversion is legal; an illegal one leaves the
    /// value alone and is what makes `AcceptInput` refuse the input with a
    /// warning.
    ///
    /// Three rules in here are not symmetric and are Valve's:
    /// **anything converts to `Void`** (by being thrown away), **anything
    /// converts to [`Input`](FieldType::Input)** (by not being touched), and
    /// **`String` converts to everything** — "everyone must convert from
    /// FIELD_STRING if possible, since parameter overrides are always passed
    /// as strings", which is the sentence that makes a map's
    /// `SetAutoExposureMax 1.5` reach a float handler.
    pub fn convert(&mut self, to: FieldType) -> bool {
        if self.field_type() == to {
            return true;
        }
        if to == FieldType::Void {
            *self = Variant::Void;
            return true;
        }
        if to == FieldType::Input {
            return true;
        }

        let converted = match (&*self, to) {
            (Variant::Int(v), FieldType::Float) => Some(Variant::Float(*v as f32)),
            (Variant::Int(v), FieldType::Bool) => Some(Variant::Bool(*v != 0)),

            // `(int)` truncates towards zero, which is not `f32::round`.
            (Variant::Float(v), FieldType::Int) => Some(Variant::Int(*v as i32)),
            (Variant::Float(v), FieldType::Bool) => Some(Variant::Bool(*v != 0.0)),

            (Variant::String(s), FieldType::Int) => Some(Variant::Int(atoi(s))),
            (Variant::String(s), FieldType::Float) => Some(Variant::Float(atof(s))),
            (Variant::String(s), FieldType::Bool) => Some(Variant::Bool(atoi(s) != 0)),
            (Variant::String(s), FieldType::Vector) => Some(Variant::Vector(parse_vector(s))),
            (Variant::String(s), FieldType::Color32) => Some(Variant::Color32(parse_color(s))),
            _ => None,
        };

        match converted {
            Some(value) => {
                *self = value;
                true
            }
            None => false,
        }
    }

    /// `variant_t::Float`. **Zero unless the value is already a float** — see
    /// the type docs.
    pub fn float(&self) -> f32 {
        match self {
            Variant::Float(v) => *v,
            _ => 0.0,
        }
    }

    /// `variant_t::Int`.
    pub fn int(&self) -> i32 {
        match self {
            Variant::Int(v) => *v,
            _ => 0,
        }
    }

    /// `variant_t::Bool`.
    pub fn bool(&self) -> bool {
        match self {
            Variant::Bool(v) => *v,
            _ => false,
        }
    }

    /// `variant_t::ToString` (`cbase.cpp:1471`) — every type prints, because
    /// `logic_case` compares its cases against whatever it is handed.
    ///
    /// The one format worth noting is the vector's: Valve prints `[x y z]`
    /// with brackets, and `%g` for floats, which is what
    /// [`parse_vector`] reads back.
    pub fn to_string(&self) -> String {
        match self {
            Variant::Void => String::new(),
            Variant::Bool(v) => match v {
                true => String::from("true"),
                false => String::from("false"),
            },
            Variant::Int(v) => v.to_string(),
            Variant::Float(v) => format_g(*v),
            Variant::String(s) => s.clone(),
            Variant::Vector(v) => {
                format!("[{} {} {}]", format_g(v.x), format_g(v.y), format_g(v.z))
            }
            Variant::Color32(c) => format!("{} {} {} {}", c[0], c[1], c[2], c[3]),
        }
    }
}

/// C's `%g`: the shorter of `%e` and `%f`, six significant digits, trailing
/// zeroes removed.
///
/// Rust's `{}` for `f32` prints the shortest round-tripping decimal, which is
/// *more* precise than `%g` and so prints `0.1` as `0.1` (agreeing) but
/// `1.0 / 3.0` as `0.33333334` where C prints `0.333333`. `logic_case`
/// compares strings, so the difference is reachable — six digits it is.
fn format_g(v: f32) -> String {
    if v == 0.0 {
        // Covers -0.0, which `%g` prints as `-0`; so does this.
        return match v.is_sign_negative() {
            true => String::from("-0"),
            false => String::from("0"),
        };
    }
    let exponent = v.abs().log10().floor() as i32;
    // `%g` uses `%e` when the exponent is < -4 or >= the precision (6).
    let mut s = match exponent < -4 || exponent >= 6 {
        true => format!("{:e}", v),
        false => format!("{:.*}", (5 - exponent).max(0) as usize, v),
    };
    if s.contains('.') && !s.contains('e') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    s
}

/// `variant_t::Convert`'s `FIELD_VECTOR` case: `sscanf("[%f %f %f]")` first,
/// then `sscanf("%f %f %f")` if that matched nothing.
///
/// Valve's test is `== 0`, i.e. *nothing* matched, so `"[1 2"` leaves the
/// first component parsed and the rest at the origin. Reproduced by reading
/// the bracketed form when the string starts with `[` and treating a failure
/// to find a number as a zero, which is what `sscanf` leaves behind.
fn parse_vector(s: &str) -> Vec3 {
    let inner = s.trim_start();
    match inner.strip_prefix('[') {
        Some(rest) => string_to_vector(rest.trim_end_matches(']')),
        None => string_to_vector(inner),
    }
}

/// `variant_t::Convert`'s `FIELD_COLOR32` case: `sscanf("%d %d %d %d")` with
/// alpha pre-set to 255, so a three-component value is opaque.
///
/// **Not [`string_to_color32`](super::keyvalue::string_to_color32)**, which is
/// `V_StringToColor32` and has the `j + 1` counting quirk. This one is a plain
/// `sscanf`, and the difference shows on a value with five components: the
/// keyvalue path falls back to opaque and this one keeps the fourth.
fn parse_color(s: &str) -> [u8; 4] {
    let mut out = [0i32, 0, 0, 255];
    for (slot, token) in out.iter_mut().zip(s.split_ascii_whitespace()) {
        *slot = atoi(token);
    }
    [out[0] as u8, out[1] as u8, out[2] as u8, out[3] as u8]
}

// ---------------------------------------------------------------------------
// connections
// ---------------------------------------------------------------------------

/// One connection: `CEventAction` (`entityoutput.h:29`).
///
/// The five fields of an output key's value —
/// `target ␛ input ␛ parameter ␛ delay ␛ times-to-fire`.
#[derive(Debug, Clone)]
pub struct EventAction {
    /// The `targetname` (or classname, or procedural name) to fire at.
    pub target: String,
    /// The input to fire. **Defaults to `"Use"`** when the field is empty,
    /// which 22 shipped connections rely on (`cbase.cpp:150`).
    pub input: String,
    /// The parameter override, if the mapper typed one. `None` means "pass the
    /// output's own value through".
    pub parameter: Option<String>,
    pub delay: f32,
    /// How many more times this connection may fire, or [`EVENT_FIRE_ALWAYS`].
    /// 2,925 of the shipped game's connections are `1`.
    pub times_to_fire: i32,
    /// `m_iIDStamp` — a serial number unique across the level, carried into
    /// the queue so that a debugger can tie an event back to the connection
    /// that posted it.
    pub id: u32,
}

impl EventAction {
    /// `CEventAction::CEventAction( const char *ActionData )`
    /// (`cbase.cpp:110`).
    ///
    /// `id` is the caller's counter; Valve's is a file-scope
    /// `s_iNextIDStamp` that never resets, which is a global this port does
    /// not need — the counter lives on the [`Server`](super::Server) and
    /// restarts with the level.
    ///
    /// Never fails. A malformed value yields empty fields and zeroes, which is
    /// what `nexttoken` plus `atof` do, and is why map data this dirty is
    /// survivable.
    pub fn parse(value: &str, id: u32) -> EventAction {
        // `cbase.cpp:128`: ESC if the value has one, a comma otherwise.
        let delimiter = match value.contains(DELIMITER) {
            true => DELIMITER,
            false => ',',
        };
        let mut fields = value.split(delimiter);
        let mut next = || fields.next().unwrap_or("");

        let target = next().to_owned();
        let input = match next() {
            "" => String::from("Use"),
            input => input.to_owned(),
        };
        let parameter = match next() {
            "" => None,
            parameter => Some(parameter.to_owned()),
        };
        let delay = match next() {
            "" => 0.0,
            delay => atof(delay),
        };
        let times_to_fire = match next() {
            "" => EVENT_FIRE_ALWAYS,
            times => match atoi(times) {
                // "0 means fire always", which is not what a mapper typing 0
                // would expect and is what the code does (`cbase.cpp:176`).
                0 => EVENT_FIRE_ALWAYS,
                times => times,
            },
        };

        EventAction {
            target,
            input,
            parameter,
            delay,
            times_to_fire,
            id,
        }
    }
}

/// One named output and everything connected to it. `CBaseEntityOutput`.
#[derive(Debug, Clone)]
pub struct Output {
    /// The output's name as the class declares it — `"OnTrigger"`.
    pub name: String,
    /// The connections, **in fire order**, which is the *reverse* of the order
    /// the entity lump lists them in. See [`Output::add`].
    pub actions: Vec<EventAction>,
}

impl Output {
    pub fn new(name: &str) -> Output {
        Output {
            name: name.to_owned(),
            actions: Vec::new(),
        }
    }

    /// `CBaseEntityOutput::AddEventAction` (`cbase.cpp:375`).
    ///
    /// > **Valve's action list is built by *prepending*.** Three lines:
    /// > `pEventAction->m_pNext = m_ActionList; m_ActionList = pEventAction;`.
    /// > `ParseKeyvalue` feeds it the output keys in lump order, so the list
    /// > `FireOutput` walks is in **reverse lump order** — the connection
    /// > Hammer wrote last is posted first.
    ///
    /// This is observable whenever two connections on one output reach the
    /// same target, because the queue is stable for equal fire times. It is
    /// reproduced by inserting at the front, and pinned by
    /// `tests::an_outputs_connections_fire_in_reverse_lump_order`.
    pub fn add(&mut self, action: EventAction) {
        self.actions.insert(0, action);
    }

    /// `CBaseEntityOutput::GetMaxDelay` (`cbase.cpp:226`) — the largest delay
    /// on any connection, or 0 if there are none.
    ///
    /// `logic_relay`'s re-fire latch is scheduled at this plus a millisecond,
    /// which is the only reason it exists.
    pub fn max_delay(&self) -> f32 {
        self.actions
            .iter()
            .map(|action| action.delay)
            .fold(0.0, f32::max)
    }

    /// `CBaseEntityOutput::NumberOfElements`.
    pub fn len(&self) -> usize {
        self.actions.len()
    }
}

// ---------------------------------------------------------------------------
// the queue
// ---------------------------------------------------------------------------

/// What an event is aimed at. The two `CEventQueue::AddEvent` overloads.
///
/// Valve's event carries both a `string_t` name and an entity pointer and
/// leaves one of them null; an enum says the same thing and makes the
/// "neither" case unrepresentable.
#[derive(Debug, Clone)]
pub enum Target {
    /// A `targetname`, a classname, or a procedural `!name`. Resolved at
    /// dispatch, not at post time, because the entity may not exist yet.
    Name(String),
    /// A direct handle. `logic_relay`'s `EnableRefire` posts one at itself.
    Entity(EntityId),
}

/// One queued event. `EventQueuePrioritizedEvent_t` (`eventqueue.h:30`).
#[derive(Debug, Clone)]
pub struct Event {
    /// `gpGlobals->curtime + fireDelay`, the sort key.
    pub fire_time: f32,
    pub target: Target,
    pub input: String,
    pub value: Variant,
    pub activator: Option<EntityId>,
    pub caller: Option<EntityId>,
    pub output_id: u32,
}

/// The global I/O queue. `CEventQueue`/`g_EventQueue`.
///
/// One per [`Server`](super::Server) rather than a file-scope global, which is
/// `PORTING.md`'s rule about singletons and is also what lets the tests run two
/// servers at once.
#[derive(Default)]
pub struct EventQueue {
    /// Sorted by [`fire_time`](Event::fire_time) ascending, **stable for
    /// equal times**.
    events: Vec<Event>,
}

impl EventQueue {
    pub fn new() -> EventQueue {
        EventQueue::default()
    }

    /// `CEventQueue::AddEvent` (`cbase.cpp:873`).
    ///
    /// Valve walks the list from the head while
    /// `pe->m_pNext->m_flFireTime > newEvent->m_flFireTime` is false — i.e.
    /// past *everything* at or before the new time — and inserts there. So
    /// events with equal fire times keep insertion order, which is the whole
    /// reason a chain of zero-delay connections runs in a defined sequence.
    /// `partition_point` finds the same index in `log n`.
    pub fn add(&mut self, event: Event) {
        let at = self
            .events
            .partition_point(|queued| queued.fire_time <= event.fire_time);
        self.events.insert(at, event);
    }

    /// `CEventQueue::CancelEvents` (`cbase.cpp:1026`) — drop everything this
    /// entity posted.
    ///
    /// Valve's version compares the caller *pointer* and then re-compares its
    /// name and classname against themselves, which can only ever be true;
    /// the extra test is dead and is not reproduced. Returns how many went.
    pub fn cancel_from(&mut self, caller: EntityId) -> usize {
        let before = self.events.len();
        self.events.retain(|event| event.caller != Some(caller));
        before - self.events.len()
    }

    /// `CEventQueue::CancelEventOn` (`cbase.cpp:1064`).
    ///
    /// Two things about the match are Valve's and are surprising:
    /// it only sees events posted at a **direct handle** (a
    /// [`Target::Name`] event aimed at this same entity is not cancelled), and
    /// the input comparison is `StringHasPrefixCaseSensitive` — a
    /// **case-sensitive prefix**, so cancelling `"En"` also cancels
    /// `"EnableRefire"`. Nothing in stage 2 calls it; it is here because
    /// [`has_pending`](EventQueue::has_pending) shares the rule and the two
    /// must not drift.
    #[allow(dead_code)]
    pub fn cancel_on(&mut self, target: EntityId, input: &str) -> usize {
        let before = self.events.len();
        self.events
            .retain(|event| !matches_pending(event, target, Some(input)));
        before - self.events.len()
    }

    /// `CEventQueue::HasEventPending` (`cbase.cpp:1099`). `None` asks about any
    /// input at all.
    #[allow(dead_code)]
    pub fn has_pending(&self, target: EntityId, input: Option<&str>) -> bool {
        self.events
            .iter()
            .any(|event| matches_pending(event, target, input))
    }

    /// Everything due at or before `now`, removed from the queue in fire
    /// order.
    ///
    /// # Why this is not `ServiceEvents`' loop
    ///
    /// `CEventQueue::ServiceEvents` (`cbase.cpp:911`) dispatches one event,
    /// deletes it, and then **restarts from the head of the list** — with a
    /// comment saying it does so "to catch any new items [that] have probably
    /// been added to the queue". The famous consequence is that a chain of
    /// eight zero-delay `logic_relay`s completes in one tick rather than
    /// eight.
    ///
    /// Restarting from the head is *equivalent to* popping the front, because
    /// the queue is sorted and [`add`](EventQueue::add) is stable: an event
    /// posted during dispatch at the current time lands after every event
    /// already due, so the head is always the next one to run. So the
    /// dispatcher pops one at a time — [`Server::service_events`] is the
    /// loop — and this returns the next due event rather than a batch, which
    /// is what keeps a handler's own new events visible in the same pass.
    pub fn pop_due(&mut self, now: f32) -> Option<Event> {
        match self.events.first() {
            Some(event) if event.fire_time <= now => Some(self.events.remove(0)),
            _ => None,
        }
    }

    /// `CEventQueue::Clear`.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Everything queued, in fire order. `CEventQueue::Dump`'s walk.
    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    /// Drops every event naming an entity that no longer exists, so that a
    /// level's queue cannot outlive the level.
    ///
    /// Valve has no equivalent and does not need one: its handles resolve to
    /// null and the event is then reported as "target entity not found". This
    /// port would do the same, so this exists only for
    /// [`Server::level_shutdown`], which drops the list wholesale.
    pub fn retain_targets(&mut self, alive: impl Fn(EntityId) -> bool) {
        self.events.retain(|event| match event.target {
            Target::Entity(id) => alive(id),
            Target::Name(_) => true,
        });
    }
}

/// The shared half of `CancelEventOn` and `HasEventPending`.
fn matches_pending(event: &Event, target: EntityId, input: Option<&str>) -> bool {
    if !matches!(event.target, Target::Entity(id) if id == target) {
        return false;
    }
    match input {
        None => true,
        Some(input) => event.input.starts_with(input),
    }
}

/// What reached an entity. `inputdata_t` (`game/server/entityinput.h`).
///
/// The value has already been through [`Variant::convert`] against the type
/// the class declared, so a handler reads it without checking — which is what
/// `variant_t`'s type-checked accessors need and is why an unconvertible value
/// never reaches one.
pub struct Input<'a> {
    /// The input name as the connection spelled it. Case is the mapper's.
    pub name: &'a str,
    pub value: Variant,
    /// Who started this chain. **Forwarded, not replaced**, by every relay it
    /// passes through — which is what makes `!activator` work several hops
    /// from the thing that moved.
    pub activator: Option<EntityId>,
    /// Who fired the output that became this input.
    pub caller: Option<EntityId>,
    /// The [`EventAction::id`] this came from.
    #[allow(dead_code)]
    pub output_id: u32,
}

/// Run-time I/O counters, the way [`LevelStats`](super::LevelStats) counts the
/// parse.
///
/// This is stage 2's progress metric and has the same job stage 1's had: say
/// exactly how much of the shipped game's entity *behaviour* the port runs.
/// An input nothing accepts is `DevMsg( 2, "unhandled input: ..." )` in the
/// original — dropped silently — so counting is this port's, not Valve's.
#[derive(Default, Clone)]
pub struct IoStats {
    /// Events taken off the queue.
    pub dispatched: usize,
    /// Inputs a class accepted.
    pub accepted: usize,
    /// Events whose target resolved to nothing at all.
    pub no_target: usize,
    /// Inputs that reached an entity which did not implement them, by name.
    pub unhandled: BTreeMap<String, usize>,
    /// Inputs refused because the value would not convert to the declared
    /// type. Valve's `!! ERROR: bad input/output link`.
    pub bad_conversion: usize,
    /// Thinks run.
    pub thinks: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(value: &str) -> EventAction {
        EventAction::parse(value, 1)
    }

    #[test]
    fn a_connection_is_five_escape_separated_fields() {
        let a = action("door\u{1b}Open\u{1b}\u{1b}1.5\u{1b}-1");
        assert_eq!(a.target, "door");
        assert_eq!(a.input, "Open");
        assert_eq!(a.parameter, None);
        assert_eq!(a.delay, 1.5);
        assert_eq!(a.times_to_fire, EVENT_FIRE_ALWAYS);

        let a = action("tonemap\u{1b}SetAutoExposureMax\u{1b}1.5\u{1b}0\u{1b}1");
        assert_eq!(a.parameter.as_deref(), Some("1.5"));
        assert_eq!(a.times_to_fire, 1);
    }

    /// The comma fallback. No shipped Portal 2 connection reaches it — see
    /// [`DELIMITER`] — but a parameter containing a comma proves why the ESC
    /// exists, so both are pinned.
    #[test]
    fn a_value_with_no_escape_falls_back_to_commas() {
        let a = action("door,Open,,0,-1");
        assert_eq!((a.target.as_str(), a.input.as_str()), ("door", "Open"));

        // …and with an ESC present, a comma is just a character.
        let a = action("hud\u{1b}AddOutput\u{1b}a,b,c\u{1b}0\u{1b}-1");
        assert_eq!(a.parameter.as_deref(), Some("a,b,c"));
    }

    /// Two defaults that are easy to miss and that shipped maps depend on.
    #[test]
    fn an_empty_input_is_use_and_a_zero_fire_count_is_always() {
        let a = action("thing\u{1b}\u{1b}\u{1b}0\u{1b}-1");
        assert_eq!(a.input, "Use", "22 shipped connections rely on this");

        let a = action("thing\u{1b}Trigger\u{1b}\u{1b}0\u{1b}0");
        assert_eq!(a.times_to_fire, EVENT_FIRE_ALWAYS);

        // A truncated value is not an error; the missing fields default.
        let a = action("thing");
        assert_eq!(a.input, "Use");
        assert_eq!(a.delay, 0.0);
        assert_eq!(a.times_to_fire, EVENT_FIRE_ALWAYS);
    }

    /// `AddEventAction` prepends, so the lump's last connection fires first.
    #[test]
    fn an_outputs_connections_fire_in_reverse_lump_order() {
        let mut output = Output::new("OnTrigger");
        output.add(action("first\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1"));
        output.add(action("second\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1"));
        output.add(action("third\u{1b}Trigger\u{1b}\u{1b}0\u{1b}-1"));

        let targets: Vec<&str> = output.actions.iter().map(|a| a.target.as_str()).collect();
        assert_eq!(targets, vec!["third", "second", "first"]);
    }

    #[test]
    fn max_delay_is_the_largest_and_zero_when_there_are_none() {
        let mut output = Output::new("OnTrigger");
        assert_eq!(output.max_delay(), 0.0);
        output.add(action("a\u{1b}Trigger\u{1b}\u{1b}0.5\u{1b}-1"));
        output.add(action("b\u{1b}Trigger\u{1b}\u{1b}2\u{1b}-1"));
        output.add(action("c\u{1b}Trigger\u{1b}\u{1b}1\u{1b}-1"));
        assert_eq!(output.max_delay(), 2.0);
    }

    #[test]
    fn the_queue_is_sorted_and_stable_for_equal_times() {
        let mut queue = EventQueue::new();
        let event = |time: f32, input: &str| Event {
            fire_time: time,
            target: Target::Name(String::from("x")),
            input: input.to_owned(),
            value: Variant::Void,
            activator: None,
            caller: None,
            output_id: 0,
        };
        queue.add(event(1.0, "late"));
        queue.add(event(0.0, "first"));
        queue.add(event(0.0, "second"));
        queue.add(event(0.5, "middle"));

        let order: Vec<String> = queue.iter().map(|e| e.input.clone()).collect();
        assert_eq!(order, vec!["first", "second", "middle", "late"]);
    }

    /// The zero-delay chain: an event posted *during* dispatch at the current
    /// time runs in the same pass, after everything already due.
    #[test]
    fn an_event_posted_at_the_current_time_runs_in_the_same_pass() {
        let mut queue = EventQueue::new();
        let event = |time: f32, input: &str| Event {
            fire_time: time,
            target: Target::Name(String::from("x")),
            input: input.to_owned(),
            value: Variant::Void,
            activator: None,
            caller: None,
            output_id: 0,
        };
        queue.add(event(0.0, "a"));
        queue.add(event(0.0, "b"));
        queue.add(event(1.0, "not yet"));

        let mut ran = Vec::new();
        let mut posted = false;
        while let Some(due) = queue.pop_due(0.0) {
            ran.push(due.input.clone());
            if due.input == "a" && !posted {
                posted = true;
                queue.add(event(0.0, "posted by a"));
            }
        }
        assert_eq!(ran, vec!["a", "b", "posted by a"]);
        assert_eq!(queue.len(), 1, "the future event is untouched");
    }

    #[test]
    fn conversions_are_valves_square_table() {
        let mut v = Variant::String(String::from("1.5"));
        assert!(v.convert(FieldType::Float));
        assert_eq!(v, Variant::Float(1.5));

        let mut v = Variant::String(String::from("3"));
        assert!(v.convert(FieldType::Bool));
        assert_eq!(v, Variant::Bool(true));

        let mut v = Variant::Float(2.9);
        assert!(v.convert(FieldType::Int));
        assert_eq!(v, Variant::Int(2), "(int) truncates, it does not round");

        // Anything to Void, and anything to FIELD_INPUT.
        let mut v = Variant::Float(1.0);
        assert!(v.convert(FieldType::Void));
        assert_eq!(v, Variant::Void);
        let mut v = Variant::Float(1.0);
        assert!(v.convert(FieldType::Input));
        assert_eq!(v, Variant::Float(1.0), "FIELD_INPUT does not touch it");

        // …but not everything converts.
        let mut v = Variant::Vector(Vec3::ZERO);
        assert!(!v.convert(FieldType::Float));
        let mut v = Variant::Void;
        assert!(!v.convert(FieldType::Float), "void is not zero");
    }

    #[test]
    fn a_string_converts_to_a_vector_with_or_without_brackets() {
        let mut v = Variant::String(String::from("[1 2 3]"));
        assert!(v.convert(FieldType::Vector));
        assert_eq!(v, Variant::Vector(Vec3::new(1.0, 2.0, 3.0)));

        let mut v = Variant::String(String::from("4 5 6"));
        assert!(v.convert(FieldType::Vector));
        assert_eq!(v, Variant::Vector(Vec3::new(4.0, 5.0, 6.0)));
    }

    /// `sscanf("%d %d %d %d")` with alpha pre-set — *not* `V_StringToColor32`.
    #[test]
    fn a_variant_colour_is_sscanf_and_not_the_keyvalue_splitter() {
        let mut v = Variant::String(String::from("1 2 3"));
        assert!(v.convert(FieldType::Color32));
        assert_eq!(v, Variant::Color32([1, 2, 3, 255]));

        // Five components: the keyvalue path would fall back to opaque.
        let mut v = Variant::String(String::from("1 2 3 4 5"));
        assert!(v.convert(FieldType::Color32));
        assert_eq!(v, Variant::Color32([1, 2, 3, 4]));
    }

    /// `logic_case` compares the *printed* value, so the float format is
    /// behaviour and not presentation.
    #[test]
    fn a_value_prints_the_way_printf_g_prints_it() {
        assert_eq!(Variant::Float(1.0).to_string(), "1");
        assert_eq!(Variant::Float(1.5).to_string(), "1.5");
        assert_eq!(Variant::Float(0.0).to_string(), "0");
        assert_eq!(Variant::Float(1.0 / 3.0).to_string(), "0.333333");
        assert_eq!(Variant::Int(-7).to_string(), "-7");
        assert_eq!(Variant::Bool(true).to_string(), "true");
        assert_eq!(Variant::Void.to_string(), "");
        assert_eq!(
            Variant::Vector(Vec3::new(1.0, 2.5, 0.0)).to_string(),
            "[1 2.5 0]"
        );
    }

    #[test]
    fn cancelling_by_caller_drops_only_that_callers_events() {
        let mut list = super::super::entity::EntityList::new();
        let class = super::super::classes::lookup("info_target").expect("registered");
        let a = list.insert(super::super::entity::Entity::new(class));
        let b = list.insert(super::super::entity::Entity::new(class));

        let mut queue = EventQueue::new();
        for caller in [a, b, a] {
            queue.add(Event {
                fire_time: 0.0,
                target: Target::Name(String::from("x")),
                input: String::from("Trigger"),
                value: Variant::Void,
                activator: None,
                caller: Some(caller),
                output_id: 0,
            });
        }
        assert_eq!(queue.cancel_from(a), 2);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.iter().next().unwrap().caller, Some(b));
    }

    /// `CancelEventOn`/`HasEventPending` see direct-handle events only, and
    /// the input match is a case-sensitive prefix.
    #[test]
    fn pending_events_match_by_handle_and_by_prefix() {
        let mut list = super::super::entity::EntityList::new();
        let class = super::super::classes::lookup("info_target").expect("registered");
        let id = list.insert(super::super::entity::Entity::new(class));

        let mut queue = EventQueue::new();
        queue.add(Event {
            fire_time: 0.0,
            target: Target::Entity(id),
            input: String::from("EnableRefire"),
            value: Variant::Void,
            activator: None,
            caller: None,
            output_id: 0,
        });
        assert!(queue.has_pending(id, None));
        assert!(queue.has_pending(id, Some("Enable")), "a prefix matches");
        assert!(!queue.has_pending(id, Some("enable")), "case sensitive");
        assert_eq!(queue.cancel_on(id, "Enable"), 1);
    }
}
