//! What is held, how long it was held for, and the bitfield that goes on the
//! command.
//!
//! `kbutton_t` (`game/client/kbutton.h`), `KeyDown`/`KeyUp`
//! (`in_main.cpp:424`, `:460`), `KeyState` (`:813`), `CalcButtonBits` (`:1738`)
//! and `GetButtonBits` (`:1771`), plus the `IN_*` set from
//! `game/shared/in_buttons.h`.
//!
//! # A button is not a bool
//!
//! It is *how much of the frame it was held for*, which is what stops a 30 Hz
//! frame from swallowing a tap, and it is *which keys are holding it*, which is
//! what stops two keys bound to `+forward` from cancelling each other. Both
//! halves are `portdocs/ENGINE_INPUT.md` §4.4's deliberate deferral, landing
//! here because [`UserCmd`](super::UserCmd) is the consumer that makes them
//! mean something.

/// One `IN_*` bit, or a set of them (`game/shared/in_buttons.h`).
///
/// **The values are external content** while a command reaches the wire or a
/// `.dem` file, so they are Valve's, not ours. The Portal 2 set is the one
/// ported: `IN_COOP_PING` and `IN_REMOTE_VIEW` are Portal 2's own
/// (`in_buttons.h:41-45`), and `INFESTED_DLL`'s reuse of bits 22-31 for
/// abilities is not — a game this port does not target overwriting bits it
/// does is exactly the kind of thing to copy by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ButtonBits(u32);

impl ButtonBits {
    pub const NONE: ButtonBits = ButtonBits(0);
    pub const ATTACK: ButtonBits = ButtonBits(1 << 0);
    pub const JUMP: ButtonBits = ButtonBits(1 << 1);
    pub const DUCK: ButtonBits = ButtonBits(1 << 2);
    pub const FORWARD: ButtonBits = ButtonBits(1 << 3);
    pub const BACK: ButtonBits = ButtonBits(1 << 4);
    pub const USE: ButtonBits = ButtonBits(1 << 5);
    pub const LEFT: ButtonBits = ButtonBits(1 << 7);
    pub const RIGHT: ButtonBits = ButtonBits(1 << 8);
    pub const MOVELEFT: ButtonBits = ButtonBits(1 << 9);
    pub const MOVERIGHT: ButtonBits = ButtonBits(1 << 10);
    pub const ATTACK2: ButtonBits = ButtonBits(1 << 11);
    pub const RELOAD: ButtonBits = ButtonBits(1 << 13);
    pub const SCORE: ButtonBits = ButtonBits(1 << 16);
    pub const SPEED: ButtonBits = ButtonBits(1 << 17);
    pub const WALK: ButtonBits = ButtonBits(1 << 18);
    pub const ZOOM: ButtonBits = ButtonBits(1 << 19);

    pub const fn contains(self, other: ButtonBits) -> bool {
        self.0 & other.0 == other.0
    }

    const fn union(self, other: ButtonBits) -> ButtonBits {
        ButtonBits(self.0 | other.0)
    }

    /// `bits |= other` — what `mv->m_nOldButtons |= IN_JUMP` says.
    pub const fn insert(self, other: ButtonBits) -> ButtonBits {
        ButtonBits(self.0 | other.0)
    }

    /// `bits &= ~other`.
    pub const fn remove(self, other: ButtonBits) -> ButtonBits {
        ButtonBits(self.0 & !other.0)
    }

    /// The bits set in both.
    pub const fn intersection(self, other: ButtonBits) -> ButtonBits {
        ButtonBits(self.0 & other.0)
    }

    /// The bits set in one and not the other — `m_nOldButtons ^ m_nButtons`,
    /// which is how `Duck` finds the press and release edges.
    pub const fn changed(self, other: ButtonBits) -> ButtonBits {
        ButtonBits(self.0 ^ other.0)
    }
}

/// A `+command` this module answers to.
///
/// Valve declares one `kbutton_t` per name at file scope in `in_main.cpp:136`
/// onwards; this is the same list as an index, so that
/// [`Buttons::bits`](Buttons::bits) is a table walk rather than twenty-one
/// hand-written `CalcButtonBits` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveButton {
    Forward,
    Back,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Left,
    Right,
    LookUp,
    LookDown,
    Speed,
    Walk,
    Strafe,
    KLook,
    Attack,
    Attack2,
    Use,
    Jump,
    Duck,
    Reload,
    Zoom,
    Score,
}

impl MoveButton {
    pub const COUNT: usize = BUTTONS.len();

    const fn index(self) -> usize {
        self as usize
    }
}

/// One row of the table: the two command spellings, the bare name they share,
/// and the `IN_*` bit the button contributes — [`ButtonBits::NONE`] for the
/// ones that are pure client-side axes and never reach the server.
pub struct ButtonSpec {
    pub down: &'static str,
    pub up: &'static str,
    pub name: &'static str,
    pub button: MoveButton,
    pub bits: ButtonBits,
}

/// Every `+command` stage 1 registers, in [`MoveButton`] order.
///
/// **Six of these carry no bit**, and that is Valve's: `+moveup`, `+movedown`,
/// `+lookup`, `+lookdown`, `+strafe` and `+klook` are client-side modifiers
/// that change how the *other* buttons are read, so `GetButtonBits`
/// (`in_main.cpp:1771`) never mentions them.
///
/// Not here, and deliberately: `+alt1`/`+alt2`, `+grenade1`/`+grenade2`,
/// `+lookspin`, `+coop_ping`, `+remote_view` and the `ducktoggle`/`speedtoggle`
/// pair. All are game features rather than movement, and each one is a bit
/// nothing would read.
pub const BUTTONS: &[ButtonSpec] = &[
    spec("forward", MoveButton::Forward, ButtonBits::FORWARD),
    spec("back", MoveButton::Back, ButtonBits::BACK),
    spec("moveleft", MoveButton::MoveLeft, ButtonBits::MOVELEFT),
    spec("moveright", MoveButton::MoveRight, ButtonBits::MOVERIGHT),
    spec("moveup", MoveButton::MoveUp, ButtonBits::NONE),
    spec("movedown", MoveButton::MoveDown, ButtonBits::NONE),
    spec("left", MoveButton::Left, ButtonBits::LEFT),
    spec("right", MoveButton::Right, ButtonBits::RIGHT),
    spec("lookup", MoveButton::LookUp, ButtonBits::NONE),
    spec("lookdown", MoveButton::LookDown, ButtonBits::NONE),
    spec("speed", MoveButton::Speed, ButtonBits::SPEED),
    spec("walk", MoveButton::Walk, ButtonBits::WALK),
    spec("strafe", MoveButton::Strafe, ButtonBits::NONE),
    spec("klook", MoveButton::KLook, ButtonBits::NONE),
    spec("attack", MoveButton::Attack, ButtonBits::ATTACK),
    spec("attack2", MoveButton::Attack2, ButtonBits::ATTACK2),
    spec("use", MoveButton::Use, ButtonBits::USE),
    spec("jump", MoveButton::Jump, ButtonBits::JUMP),
    spec("duck", MoveButton::Duck, ButtonBits::DUCK),
    spec("reload", MoveButton::Reload, ButtonBits::RELOAD),
    spec("zoom", MoveButton::Zoom, ButtonBits::ZOOM),
    spec("score", MoveButton::Score, ButtonBits::SCORE),
];

/// The table is written with both spellings expanded rather than `+`-prefixed
/// at runtime, because [`CommandSpec`](crate::engine::console::CommandSpec)
/// takes a `&'static str` and these are what gets registered.
const fn spec(name: &'static str, button: MoveButton, bits: ButtonBits) -> ButtonSpec {
    // `concat!` cannot take a binding, so the two spellings are matched rather
    // than built. One arm per name; the compiler checks exhaustiveness for us
    // the moment a row is added without one.
    let (down, up) = match button {
        MoveButton::Forward => ("+forward", "-forward"),
        MoveButton::Back => ("+back", "-back"),
        MoveButton::MoveLeft => ("+moveleft", "-moveleft"),
        MoveButton::MoveRight => ("+moveright", "-moveright"),
        MoveButton::MoveUp => ("+moveup", "-moveup"),
        MoveButton::MoveDown => ("+movedown", "-movedown"),
        MoveButton::Left => ("+left", "-left"),
        MoveButton::Right => ("+right", "-right"),
        MoveButton::LookUp => ("+lookup", "-lookup"),
        MoveButton::LookDown => ("+lookdown", "-lookdown"),
        MoveButton::Speed => ("+speed", "-speed"),
        MoveButton::Walk => ("+walk", "-walk"),
        MoveButton::Strafe => ("+strafe", "-strafe"),
        MoveButton::KLook => ("+klook", "-klook"),
        MoveButton::Attack => ("+attack", "-attack"),
        MoveButton::Attack2 => ("+attack2", "-attack2"),
        MoveButton::Use => ("+use", "-use"),
        MoveButton::Jump => ("+jump", "-jump"),
        MoveButton::Duck => ("+duck", "-duck"),
        MoveButton::Reload => ("+reload", "-reload"),
        MoveButton::Zoom => ("+zoom", "-zoom"),
        MoveButton::Score => ("+score", "-score"),
    };
    ButtonSpec {
        down,
        up,
        name,
        button,
        bits,
    }
}

/// One button a `+command` holds down. `kbutton_t` (`game/client/kbutton.h`).
///
/// **`down` is why a `+command` carries an argument.** It records up to two of
/// the keys holding this button, so that two keys bound to `+forward` do not
/// cancel each other: releasing one leaves the other holding it. Without it,
/// `bind UPARROW +forward` alongside `bind w +forward` makes tapping either one
/// stop the other.
///
/// One divergence from the C++, and it fixes a latent bug: Valve stores the
/// holders as `int` with **0 meaning empty**, so button code 0 could never hold
/// anything. This uses `Option`, and `None` is the only empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KButton {
    /// The indices currently holding this down. `Some(-1)` is the holder for a
    /// `+command` typed with no argument, which is what Valve's `k = -1` is.
    down: [Option<i32>; 2],
    /// `state & 1`. **The authority on whether the button is down** — not
    /// `down`, which a bare `-command` empties without this following it in
    /// every case.
    held: bool,
    /// `state & 2` — pressed since the last read.
    pressed: bool,
    /// `state & 4` — released since the last read.
    released: bool,
}

impl KButton {
    /// `KeyDown` (`in_main.cpp:424`).
    pub fn press(&mut self, index: Option<i32>) {
        let index = index.or(Some(-1));
        if self.down[0] == index || self.down[1] == index {
            return; // repeating key
        }
        if self.down[0].is_none() {
            self.down[0] = index;
        } else if self.down[1].is_none() {
            self.down[1] = index;
        } else {
            // Valve warns and drops a third holder; two is enough to make the
            // two-keys-one-command case work, which is all this is for.
            return;
        }
        if self.held {
            return; // still down
        }
        self.held = true;
        self.pressed = true;
    }

    /// `KeyUp` (`in_main.cpp:460`).
    ///
    /// A `-command` typed with **no argument** releases unconditionally —
    /// Valve's `if ( !c || !c[0] )` branch, which sets `state = 4` outright and
    /// so drops the pressed bit along with the down bit. That is what makes
    /// typing `-forward` at the console a way out of a stuck key.
    pub fn release(&mut self, index: Option<i32>) {
        let Some(index) = index else {
            self.down = [None; 2];
            self.held = false;
            self.pressed = false;
            self.released = true;
            return;
        };
        if self.down[0] == Some(index) {
            self.down[0] = None;
        } else if self.down[1] == Some(index) {
            self.down[1] = None;
        } else {
            return; // key up without a corresponding down
        }
        if self.down[0].is_some() || self.down[1].is_some() {
            return; // some other key is still holding it down
        }
        if !self.held {
            return; // still up
        }
        self.held = false;
        self.released = true;
    }

    /// Whether the button is held right now. `state & 1`.
    pub fn is_down(&self) -> bool {
        self.held
    }

    /// `KeyState` (`in_main.cpp:813`): **the fraction of the frame the button
    /// was held for.**
    ///
    /// 1.0 held throughout, 0.5 pressed and still held, 0.25 pressed *and*
    /// released within one frame, 0.75 released and re-pressed, 0.0 otherwise.
    /// This is what makes a tap shorter than a frame move the player at all.
    ///
    /// **Destructive**, exactly as the original is: it clears both impulse
    /// bits, so a second call in the same frame answers differently. Valve gets
    /// away with one caller per button per `CreateMove`; so does this, and the
    /// single-caller property is worth keeping.
    pub fn key_state(&mut self) -> f32 {
        let value = match (self.pressed, self.released, self.held) {
            // Pressed and held this frame.
            (true, false, held) => held as u8 as f32 * 0.5,
            // Released this frame, or held for none of it.
            (false, true, _) => 0.0,
            // Held the entire frame.
            (false, false, held) => held as u8 as f32,
            // Released and re-pressed.
            (true, true, true) => 0.75,
            // Pressed and released within the frame.
            (true, true, false) => 0.25,
        };
        self.pressed = false;
        self.released = false;
        value
    }

    /// `CalcButtonBits` (`in_main.cpp:1738`): contributes its bit when the
    /// button is **down or was pressed since the last read**, so a tap inside
    /// one frame still registers.
    ///
    /// `reset` clears the *pressed* bit and nothing else — Valve's
    /// `state &= ~2`. The released bit survives, because only
    /// [`key_state`](KButton::key_state) clears that, and for a button nothing
    /// reads that way it simply accumulates and is never looked at.
    fn bits(&mut self, reset: bool) -> bool {
        let set = self.held || self.pressed;
        if reset {
            self.pressed = false;
        }
        set
    }
}

/// Every `kbutton_t` the client owns.
///
/// One player. `PerUserInput_t`'s split-screen arrays are not ported; see
/// `portdocs/CLIENT.md` §5.
#[derive(Debug, Clone, Copy)]
pub struct Buttons {
    state: [KButton; MoveButton::COUNT],
}

impl Default for Buttons {
    fn default() -> Buttons {
        Buttons {
            state: [KButton::default(); MoveButton::COUNT],
        }
    }
}

impl Buttons {
    /// Applies one `+name`/`-name` command. True if `name` was one of ours.
    ///
    /// `name` is the command without its sign, and `index` is the button-index
    /// argument the binding carried — `None` when a bare `+forward` was typed.
    pub fn apply(&mut self, name: &str, down: bool, index: Option<i32>) -> bool {
        let name = name.to_ascii_lowercase();
        let Some(spec) = BUTTONS.iter().find(|spec| spec.name == name) else {
            return false;
        };
        let button = &mut self.state[spec.button.index()];
        match down {
            true => button.press(index),
            false => button.release(index),
        }
        true
    }

    pub fn is_down(&self, button: MoveButton) -> bool {
        self.state[button.index()].is_down()
    }

    /// [`KButton::key_state`], and destructive for the same reason.
    pub fn key_state(&mut self, button: MoveButton) -> f32 {
        self.state[button.index()].key_state()
    }

    /// `CInput::GetButtonBits` (`in_main.cpp:1771`).
    ///
    /// **Call this after the `key_state` reads, not before.** The two clear
    /// different bits — `key_state` clears both impulses, this clears only
    /// `pressed` — so with Valve's ordering (`CreateMove` computes the movement
    /// axes first) a tap shorter than a frame contributes to `forwardmove` and
    /// *not* to `IN_FORWARD`. Reverse the order and it contributes to both,
    /// which is a difference the server would see.
    pub fn bits(&mut self, reset: bool) -> ButtonBits {
        let mut bits = ButtonBits::NONE;
        for spec in BUTTONS {
            if self.state[spec.button.index()].bits(reset) {
                bits = bits.union(spec.bits);
            }
        }
        bits
    }

    /// `CInput::ClearStates` — focus loss must not leave the player walking.
    pub fn clear(&mut self) {
        *self = Buttons::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_indexed_by_its_own_enum() {
        for (index, spec) in BUTTONS.iter().enumerate() {
            assert_eq!(
                spec.button.index(),
                index,
                "`{}` is out of order",
                spec.name
            );
            assert_eq!(spec.down, format!("+{}", spec.name));
            assert_eq!(spec.up, format!("-{}", spec.name));
        }
    }

    #[test]
    fn two_keys_bound_to_one_command_do_not_cancel_each_other() {
        // The whole reason a `+command` carries the index of the button that
        // sent it. `bind w +forward` and `bind UPARROW +forward`: hold both,
        // release one, keep walking.
        let mut buttons = Buttons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", true, Some(20));
        assert!(buttons.is_down(MoveButton::Forward));

        buttons.apply("forward", false, Some(20));
        assert!(
            buttons.is_down(MoveButton::Forward),
            "the other key is still holding it"
        );

        buttons.apply("forward", false, Some(10));
        assert!(!buttons.is_down(MoveButton::Forward));
    }

    #[test]
    fn a_release_for_a_button_that_never_pressed_is_ignored() {
        let mut buttons = Buttons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", false, Some(99));
        assert!(
            buttons.is_down(MoveButton::Forward),
            "key up without a matching down"
        );
    }

    #[test]
    fn a_repeated_press_from_the_same_button_is_not_a_second_holder() {
        let mut buttons = Buttons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", false, Some(10));
        assert!(
            !buttons.is_down(MoveButton::Forward),
            "one press, one release"
        );
    }

    /// Valve's `if ( !c || !c[0] )` branch: typing `-forward` at the console
    /// releases regardless of who was holding it, which is the way out of a
    /// stuck movement key.
    #[test]
    fn a_bare_minus_command_releases_unconditionally() {
        let mut buttons = Buttons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", true, Some(20));
        buttons.apply("forward", false, None);
        assert!(!buttons.is_down(MoveButton::Forward));
    }

    #[test]
    fn an_unknown_command_is_refused_rather_than_silently_dropped() {
        let mut buttons = Buttons::default();
        assert!(!buttons.apply("noclip", true, Some(1)));
    }

    #[test]
    fn a_button_held_for_the_whole_frame_is_worth_one() {
        let mut button = KButton::default();
        button.press(Some(1));
        assert_eq!(button.key_state(), 0.5, "pressed this frame");
        assert_eq!(button.key_state(), 1.0, "and held through the next");
    }

    /// The reason any of this exists: a key pressed and released between two
    /// frames still moves the player.
    #[test]
    fn a_tap_shorter_than_a_frame_is_worth_a_quarter() {
        let mut button = KButton::default();
        button.press(Some(1));
        button.release(Some(1));
        assert_eq!(button.key_state(), 0.25);
        assert_eq!(button.key_state(), 0.0, "and nothing afterwards");
    }

    #[test]
    fn a_release_and_a_re_press_in_one_frame_is_worth_three_quarters() {
        let mut button = KButton::default();
        button.press(Some(1));
        button.key_state();
        button.release(Some(1));
        button.press(Some(1));
        assert_eq!(button.key_state(), 0.75);
    }

    #[test]
    fn releasing_is_worth_nothing_for_the_frame_it_happens_in() {
        let mut button = KButton::default();
        button.press(Some(1));
        button.key_state();
        button.release(Some(1));
        assert_eq!(button.key_state(), 0.0);
    }

    #[test]
    fn a_tap_reaches_the_bitfield_when_nothing_read_it_first() {
        let mut buttons = Buttons::default();
        buttons.apply("attack", true, Some(1));
        buttons.apply("attack", false, Some(1));
        assert!(buttons.bits(true).contains(ButtonBits::ATTACK));
        assert!(
            !buttons.bits(true).contains(ButtonBits::ATTACK),
            "and only once"
        );
    }

    /// The ordering trap `Buttons::bits` documents: `key_state` clears the
    /// pressed bit that `bits` would have read.
    #[test]
    fn a_tap_that_key_state_already_read_does_not_reach_the_bitfield() {
        let mut buttons = Buttons::default();
        buttons.apply("forward", true, Some(1));
        buttons.apply("forward", false, Some(1));
        assert_eq!(buttons.key_state(MoveButton::Forward), 0.25);
        assert!(!buttons.bits(true).contains(ButtonBits::FORWARD));
    }

    #[test]
    fn the_axis_only_buttons_contribute_no_bits() {
        let mut buttons = Buttons::default();
        for name in [
            "moveup", "movedown", "lookup", "lookdown", "strafe", "klook",
        ] {
            assert!(buttons.apply(name, true, Some(1)), "`{name}` is not bound");
        }
        assert_eq!(buttons.bits(true), ButtonBits::NONE);
    }

    #[test]
    fn a_held_button_keeps_its_bit_across_frames() {
        let mut buttons = Buttons::default();
        buttons.apply("jump", true, Some(1));
        assert!(buttons.bits(true).contains(ButtonBits::JUMP));
        assert!(buttons.bits(true).contains(ButtonBits::JUMP));
        buttons.apply("jump", false, Some(1));
        assert!(!buttons.bits(true).contains(ButtonBits::JUMP));
    }
}
