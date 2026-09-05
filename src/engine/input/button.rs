//! Buttons: every binary input, in one flat, densely indexed space.
//!
//! Replaces `public/inputsystem/ButtonCode.h`'s `ButtonCode_t` and the name
//! table at `inputsystem/key_translation.cpp:357`.
//! `portdocs/ENGINE_INPUT.md` §4.2 explains what survived and what did not:
//!
//! - **Kept:** that every binary input is *one* flat, densely indexed space.
//!   That is what lets stage 3's binding table be an array and the down-state
//!   be a bitset, and it is why a controller button will bind to `+forward`
//!   with no special case anywhere.
//! - **Discarded:** the arithmetic-in-macros encoding
//!   (`JOYSTICK_BUTTON( joy, button )` and friends), and `JOYSTICK_AXIS_BUTTON`
//!   — an analog axis synthesized into a pair of fake buttons, which is a
//!   workaround for a binding system that could not express axes. `gilrs`
//!   reports axes as axes.
//!
//! The **names** are a different matter: `bind "w" "+forward"` lives in shipped
//! `.cfg` files and `scripts/kb_def.lst`, so `s_pButtonCodeName` is a fixed
//! external format in `PORTING.md`'s sense. The mechanism (a `const char *[]`
//! indexed by the enum) is ours to modernize; the strings are transcribed
//! verbatim.
//!
//! # Deliberate divergences from Valve's table
//!
//! | | Valve | Here | Why |
//! |---|---|---|---|
//! | `KEY_NONE` | code 0, name `""` | absent | "no button" is `Option<Button>`. |
//! | `KEY_CAPSLOCKTOGGLE`, `KEY_NUMLOCKTOGGLE`, `KEY_SCROLLLOCKTOGGLE` | codes 104-106 | absent | Not keys: vgui toggle-*state* pseudo-buttons, set only by `CInputWin32::UpdateToggleButtonState`. Valve's own table comments "FIXME: [...] What are these for?!". No `winit` event produces them. |
//! | `KEY_LWIN`/`KEY_RWIN` | `"COMMAND"` for both on OSX, `"LWIN"`/`"RWIN"` elsewhere | always `"LWIN"`/`"RWIN"` | Two buttons sharing one name cannot round-trip. `"COMMAND"` is still *accepted* by [`Button::from_name`], which is what `ButtonCode_StringToButtonCode` does on non-OSX (`key_translation.cpp:1178`), so a `.cfg` written on a Mac still binds. |
//!
//! That leaves Valve's 107 key codes as 103 real keys, and its 7 mouse codes
//! unchanged — the two fake wheel buttons included, because `bind MWHEELUP
//! +jump` is real content.

/// A key, by **physical position** — not by what the keycap says.
///
/// **This is a deliberate divergence, and it is the one most likely to
/// surprise** (`portdocs/ENGINE_INPUT.md` §7). Valve's POSIX path collapses
/// scan code and virtual code into one (`PollInputState_Linux` literally does
/// `ButtonCode_t scanCode = virtualCode`), so on an AZERTY keyboard Valve's
/// `bind w +forward` binds the key *labelled* W — which is where Q sits on a
/// QWERTY board, and WASD stops being a square.
///
/// [`Key::W`] here is `winit`'s `KeyCode::KeyW`: the position, whatever the
/// layout calls it. `winit`'s own `KeyEvent` docs recommend exactly this for
/// games. The logical key is what text entry and key *display* will use, and
/// neither exists yet.
///
/// The declaration order is Valve's `KEY_*` order, because [`Key::index`] is
/// the discriminant and the name table is indexed by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Pad0,
    Pad1,
    Pad2,
    Pad3,
    Pad4,
    Pad5,
    Pad6,
    Pad7,
    Pad8,
    Pad9,
    PadDivide,
    PadMultiply,
    PadMinus,
    PadPlus,
    PadEnter,
    PadDecimal,
    LeftBracket,
    RightBracket,
    Semicolon,
    Apostrophe,
    Backquote,
    Comma,
    Period,
    Slash,
    Backslash,
    Minus,
    Equal,
    Enter,
    Space,
    Backspace,
    Tab,
    CapsLock,
    NumLock,
    Escape,
    ScrollLock,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    /// `KEY_BREAK`, named `"PAUSE"`. `winit` calls it `KeyCode::Pause`.
    Break,
    LeftShift,
    RightShift,
    LeftAlt,
    RightAlt,
    LeftControl,
    RightControl,
    /// `KEY_LWIN`. Command on macOS, Super/Windows elsewhere.
    LeftSuper,
    /// `KEY_RWIN`.
    RightSuper,
    /// `KEY_APP` — the context-menu key.
    App,
    Up,
    Left,
    Down,
    Right,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// A mouse button, including the two fake ones the wheel produces.
///
/// `MOUSE_WHEEL_UP`/`MOUSE_WHEEL_DOWN` are pressed and released by a wheel
/// notch rather than by a physical button. They are kept because they are
/// content: `bind MWHEELUP +jump` appears in shipped configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// `MOUSE_4`, which `winit` calls `MouseButton::Back`.
    Mouse4,
    /// `MOUSE_5`, which `winit` calls `MouseButton::Forward`.
    Mouse5,
    WheelUp,
    WheelDown,
}

/// Any binary input.
///
/// Stage 5 adds `Gamepad { pad: u8, button: GamepadButton }` here; nothing else
/// changes, because everything downstream indexes rather than matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Key(Key),
    Mouse(MouseButton),
}

/// Key codes paired with their names, in `ButtonCode_t` order.
///
/// One table rather than two parallel ones: the name and the index are the same
/// fact twice, and `names_are_in_button_code_order` asserts the pairing has not
/// drifted.
const KEYS: &[(Key, &str)] = &[
    (Key::Num0, "0"),
    (Key::Num1, "1"),
    (Key::Num2, "2"),
    (Key::Num3, "3"),
    (Key::Num4, "4"),
    (Key::Num5, "5"),
    (Key::Num6, "6"),
    (Key::Num7, "7"),
    (Key::Num8, "8"),
    (Key::Num9, "9"),
    (Key::A, "a"),
    (Key::B, "b"),
    (Key::C, "c"),
    (Key::D, "d"),
    (Key::E, "e"),
    (Key::F, "f"),
    (Key::G, "g"),
    (Key::H, "h"),
    (Key::I, "i"),
    (Key::J, "j"),
    (Key::K, "k"),
    (Key::L, "l"),
    (Key::M, "m"),
    (Key::N, "n"),
    (Key::O, "o"),
    (Key::P, "p"),
    (Key::Q, "q"),
    (Key::R, "r"),
    (Key::S, "s"),
    (Key::T, "t"),
    (Key::U, "u"),
    (Key::V, "v"),
    (Key::W, "w"),
    (Key::X, "x"),
    (Key::Y, "y"),
    (Key::Z, "z"),
    // The numpad names are the *unshifted* legends, which is why they read as
    // navigation keys: `KP_INS` is the 0 key, `KP_END` the 1.
    (Key::Pad0, "KP_INS"),
    (Key::Pad1, "KP_END"),
    (Key::Pad2, "KP_DOWNARROW"),
    (Key::Pad3, "KP_PGDN"),
    (Key::Pad4, "KP_LEFTARROW"),
    (Key::Pad5, "KP_5"),
    (Key::Pad6, "KP_RIGHTARROW"),
    (Key::Pad7, "KP_HOME"),
    (Key::Pad8, "KP_UPARROW"),
    (Key::Pad9, "KP_PGUP"),
    (Key::PadDivide, "KP_SLASH"),
    (Key::PadMultiply, "KP_MULTIPLY"),
    (Key::PadMinus, "KP_MINUS"),
    (Key::PadPlus, "KP_PLUS"),
    (Key::PadEnter, "KP_ENTER"),
    (Key::PadDecimal, "KP_DEL"),
    (Key::LeftBracket, "["),
    (Key::RightBracket, "]"),
    (Key::Semicolon, "SEMICOLON"),
    (Key::Apostrophe, "'"),
    (Key::Backquote, "`"),
    (Key::Comma, ","),
    (Key::Period, "."),
    (Key::Slash, "/"),
    (Key::Backslash, "\\"),
    (Key::Minus, "-"),
    (Key::Equal, "="),
    (Key::Enter, "ENTER"),
    (Key::Space, "SPACE"),
    (Key::Backspace, "BACKSPACE"),
    (Key::Tab, "TAB"),
    (Key::CapsLock, "CAPSLOCK"),
    (Key::NumLock, "NUMLOCK"),
    (Key::Escape, "ESCAPE"),
    (Key::ScrollLock, "SCROLLLOCK"),
    (Key::Insert, "INS"),
    (Key::Delete, "DEL"),
    (Key::Home, "HOME"),
    (Key::End, "END"),
    (Key::PageUp, "PGUP"),
    (Key::PageDown, "PGDN"),
    (Key::Break, "PAUSE"),
    // Unprefixed means left, which is why `bind SHIFT` is the left shift.
    (Key::LeftShift, "SHIFT"),
    (Key::RightShift, "RSHIFT"),
    (Key::LeftAlt, "ALT"),
    (Key::RightAlt, "RALT"),
    (Key::LeftControl, "CTRL"),
    (Key::RightControl, "RCTRL"),
    (Key::LeftSuper, "LWIN"),
    (Key::RightSuper, "RWIN"),
    (Key::App, "APP"),
    (Key::Up, "UPARROW"),
    (Key::Left, "LEFTARROW"),
    (Key::Down, "DOWNARROW"),
    (Key::Right, "RIGHTARROW"),
    (Key::F1, "F1"),
    (Key::F2, "F2"),
    (Key::F3, "F3"),
    (Key::F4, "F4"),
    (Key::F5, "F5"),
    (Key::F6, "F6"),
    (Key::F7, "F7"),
    (Key::F8, "F8"),
    (Key::F9, "F9"),
    (Key::F10, "F10"),
    (Key::F11, "F11"),
    (Key::F12, "F12"),
];

/// Mouse codes paired with their names, in `ButtonCode_t` order.
const MICE: &[(MouseButton, &str)] = &[
    (MouseButton::Left, "MOUSE1"),
    (MouseButton::Right, "MOUSE2"),
    (MouseButton::Middle, "MOUSE3"),
    (MouseButton::Mouse4, "MOUSE4"),
    (MouseButton::Mouse5, "MOUSE5"),
    (MouseButton::WheelUp, "MWHEELUP"),
    (MouseButton::WheelDown, "MWHEELDOWN"),
];

/// `"COMMAND"` is what a `.cfg` written on a Mac calls the left Command key.
///
/// `ButtonCode_StringToButtonCode` (`key_translation.cpp:1178`) special-cases
/// it in the same direction, for the same reason.
const COMMAND_ALIAS: &str = "COMMAND";

impl Key {
    /// `KEY_COUNT` minus `KEY_NONE` and the three toggle pseudo-keys: 103.
    pub const COUNT: usize = KEYS.len();

    /// Dense, `0..COUNT`, in `ButtonCode_t` order.
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        KEYS[self.index()].1
    }
}

impl MouseButton {
    pub const COUNT: usize = MICE.len();

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        MICE[self.index()].1
    }
}

impl Button {
    /// How many buttons there are — the length of every per-button array.
    pub const COUNT: usize = Key::COUNT + MouseButton::COUNT;

    /// Dense, `0..COUNT`. Keys first, then mice, exactly as `ButtonCode_t`
    /// laid out `KEY_FIRST..KEY_LAST` then `MOUSE_FIRST..MOUSE_LAST`.
    pub fn index(self) -> usize {
        match self {
            Button::Key(key) => key.index(),
            Button::Mouse(mouse) => Key::COUNT + mouse.index(),
        }
    }

    /// The inverse of [`index`](Button::index).
    ///
    /// Stage 3's binding table is an array of `Button::COUNT` command strings,
    /// and reading one back out is this.
    #[allow(dead_code)] // stage 3: `bind`/`unbind` walking the binding table
    pub fn from_index(index: usize) -> Option<Button> {
        if index < Key::COUNT {
            Some(Button::Key(KEYS[index].0))
        } else {
            MICE.get(index - Key::COUNT)
                .map(|&(mouse, _)| Button::Mouse(mouse))
        }
    }

    /// The name a `.cfg` file uses: `"w"`, `"MOUSE1"`, `"MWHEELUP"`,
    /// `"SEMICOLON"`.
    ///
    /// `ButtonCode_ButtonCodeToString` (`key_translation.cpp:1124`).
    #[allow(dead_code)] // stage 3: `bind` with no arguments lists the bindings
    pub fn name(self) -> &'static str {
        match self {
            Button::Key(key) => key.name(),
            Button::Mouse(mouse) => mouse.name(),
        }
    }

    /// Parses a name from a `.cfg` file. Case-insensitive, as Valve's
    /// `Q_stricmp` loop is (`key_translation.cpp:1197`).
    ///
    /// The empty string is not a button: it is `KEY_NONE`'s name, and Valve
    /// rejects it before the loop.
    pub fn from_name(name: &str) -> Option<Button> {
        if name.is_empty() {
            return None;
        }
        if name.eq_ignore_ascii_case(COMMAND_ALIAS) {
            return Some(Button::Key(Key::LeftSuper));
        }
        if let Some(&(key, _)) = KEYS.iter().find(|(_, n)| name.eq_ignore_ascii_case(n)) {
            return Some(Button::Key(key));
        }
        MICE.iter()
            .find(|(_, n)| name.eq_ignore_ascii_case(n))
            .map(|&(mouse, _)| Button::Mouse(mouse))
    }

    /// Every button, in index order. Test and diagnostic use.
    pub fn all() -> impl Iterator<Item = Button> {
        (0..Button::COUNT).filter_map(Button::from_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is indexed by the discriminant everywhere, so a variant
    /// inserted in the middle without moving its row silently renames every
    /// key after it.
    #[test]
    fn names_are_in_button_code_order() {
        for (index, &(key, _)) in KEYS.iter().enumerate() {
            assert_eq!(key.index(), index, "{key:?}");
        }
        for (index, &(mouse, _)) in MICE.iter().enumerate() {
            assert_eq!(mouse.index(), index, "{mouse:?}");
        }
    }

    #[test]
    fn indices_are_dense_and_round_trip() {
        let mut seen = vec![false; Button::COUNT];
        for button in Button::all() {
            let index = button.index();
            assert!(!seen[index], "{button:?} collides at {index}");
            seen[index] = true;
            assert_eq!(Button::from_index(index), Some(button));
        }
        assert!(seen.into_iter().all(|s| s), "the space has a hole in it");
        assert_eq!(Button::from_index(Button::COUNT), None);
    }

    /// `bind` writes a name and reads it back; a name that does not survive the
    /// round trip is a binding that silently disappears from a `.cfg`.
    #[test]
    fn every_name_round_trips() {
        for button in Button::all() {
            let name = button.name();
            assert!(!name.is_empty(), "{button:?} has no name");
            assert_eq!(Button::from_name(name), Some(button), "{name}");
        }
    }

    #[test]
    fn no_two_buttons_share_a_name() {
        let mut names: Vec<&str> = Button::all().map(Button::name).collect();
        names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
        let count = names.len();
        names.dedup_by_key(|name| name.to_ascii_lowercase());
        assert_eq!(names.len(), count, "a name is used twice");
    }

    #[test]
    fn names_are_the_ones_valve_shipped() {
        // Spot checks against `s_pButtonCodeName` (`key_translation.cpp:357`),
        // chosen for the ones that are not guessable.
        assert_eq!(Button::Key(Key::Pad0).name(), "KP_INS");
        assert_eq!(Button::Key(Key::PadDecimal).name(), "KP_DEL");
        assert_eq!(Button::Key(Key::Break).name(), "PAUSE");
        assert_eq!(Button::Key(Key::Semicolon).name(), "SEMICOLON");
        assert_eq!(Button::Key(Key::Apostrophe).name(), "'");
        assert_eq!(Button::Key(Key::LeftShift).name(), "SHIFT");
        assert_eq!(Button::Key(Key::Insert).name(), "INS");
        assert_eq!(Button::Key(Key::Up).name(), "UPARROW");
        assert_eq!(Button::Mouse(MouseButton::Left).name(), "MOUSE1");
        assert_eq!(Button::Mouse(MouseButton::WheelUp).name(), "MWHEELUP");
    }

    #[test]
    fn parsing_a_name_is_case_insensitive() {
        assert_eq!(Button::from_name("W"), Some(Button::Key(Key::W)));
        assert_eq!(Button::from_name("w"), Some(Button::Key(Key::W)));
        assert_eq!(
            Button::from_name("mouse1"),
            Some(Button::Mouse(MouseButton::Left))
        );
        assert_eq!(Button::from_name("kp_ins"), Some(Button::Key(Key::Pad0)));
    }

    #[test]
    fn command_is_an_accepted_alias_for_the_left_super_key() {
        // A `.cfg` written on a Mac spells `KEY_LWIN` "COMMAND"; it must still
        // bind, but `name()` never produces it -- see the module docs.
        assert_eq!(
            Button::from_name("COMMAND"),
            Some(Button::Key(Key::LeftSuper))
        );
        assert_eq!(Button::Key(Key::LeftSuper).name(), "LWIN");
    }

    #[test]
    fn the_empty_name_is_not_a_button() {
        // It is `KEY_NONE`'s name in Valve's table, and matching it would make
        // a malformed `bind ""` line bind something.
        assert_eq!(Button::from_name(""), None);
        assert_eq!(Button::from_name("NOSUCHKEY"), None);
    }

    #[test]
    fn the_space_is_the_size_valve_shipped_minus_the_pseudo_keys() {
        assert_eq!(
            Key::COUNT,
            103,
            "107 key codes, less KEY_NONE and the 3 toggles"
        );
        assert_eq!(MouseButton::COUNT, 7, "MOUSE_COUNT, wheel buttons included");
        assert_eq!(Button::COUNT, 110);
    }
}
