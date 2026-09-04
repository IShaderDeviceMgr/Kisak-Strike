//! `winit` events into [`input::Event`](crate::engine::input::Event).
//!
//! This is all that is left of `inputsystem/` (10,649 lines) and
//! `key_translation.cpp`'s three platform scancode tables: `winit` hands over
//! the physical key, the logical key and the text of one key press in one
//! struct, so the layer that reconstructed those from SDL — and worked around
//! SDL reporting the same press twice — has nothing to do
//! (`portdocs/ENGINE_INPUT.md` §4.1).
//!
//! **Translation only.** No state, no policy, no bindings: this file is a
//! lookup and `window/`'s event arms are a `match`. Everything that remembers
//! anything lives in [`input`](crate::engine::input), which names no
//! windowing type and is testable without a window.

use winit::keyboard::KeyCode;

use crate::engine::input::{Key, MouseButton};

/// `winit`'s physical key positions paired with ours.
///
/// **Physical, never logical** — `KeyCode::KeyW` is the key where W sits on a
/// US layout, which is where Z sits on AZERTY. Binding by position is what
/// keeps WASD a square on every layout, and it is a deliberate divergence from
/// Valve's POSIX path, which collapsed scancode and virtual code into one. See
/// [`Key`]'s docs.
///
/// Codes with no row are not mapped and produce no event: F13-F35, the media
/// and browser keys, the international and IME keys (`IntlBackslash`, `IntlRo`,
/// `IntlYen`, `Lang1`-`Lang5`, `Convert`, `KanaMode`, the kana toggles), the
/// numpad's rarer legends (`NumpadComma`, `NumpadEqual`, `NumpadHash`,
/// `NumpadStar`, `NumpadParen*`, the memory keys), `Fn`/`FnLock`,
/// `PrintScreen`, `Power`/`Sleep`/`WakeUp` and the editing keys (`Copy`,
/// `Cut`, `Paste`, `Undo`, …). Valve's `ButtonCode_t` has no code for any of
/// them either.
const KEYS: &[(KeyCode, Key)] = &[
    (KeyCode::Digit0, Key::Num0),
    (KeyCode::Digit1, Key::Num1),
    (KeyCode::Digit2, Key::Num2),
    (KeyCode::Digit3, Key::Num3),
    (KeyCode::Digit4, Key::Num4),
    (KeyCode::Digit5, Key::Num5),
    (KeyCode::Digit6, Key::Num6),
    (KeyCode::Digit7, Key::Num7),
    (KeyCode::Digit8, Key::Num8),
    (KeyCode::Digit9, Key::Num9),
    (KeyCode::KeyA, Key::A),
    (KeyCode::KeyB, Key::B),
    (KeyCode::KeyC, Key::C),
    (KeyCode::KeyD, Key::D),
    (KeyCode::KeyE, Key::E),
    (KeyCode::KeyF, Key::F),
    (KeyCode::KeyG, Key::G),
    (KeyCode::KeyH, Key::H),
    (KeyCode::KeyI, Key::I),
    (KeyCode::KeyJ, Key::J),
    (KeyCode::KeyK, Key::K),
    (KeyCode::KeyL, Key::L),
    (KeyCode::KeyM, Key::M),
    (KeyCode::KeyN, Key::N),
    (KeyCode::KeyO, Key::O),
    (KeyCode::KeyP, Key::P),
    (KeyCode::KeyQ, Key::Q),
    (KeyCode::KeyR, Key::R),
    (KeyCode::KeyS, Key::S),
    (KeyCode::KeyT, Key::T),
    (KeyCode::KeyU, Key::U),
    (KeyCode::KeyV, Key::V),
    (KeyCode::KeyW, Key::W),
    (KeyCode::KeyX, Key::X),
    (KeyCode::KeyY, Key::Y),
    (KeyCode::KeyZ, Key::Z),
    (KeyCode::Numpad0, Key::Pad0),
    (KeyCode::Numpad1, Key::Pad1),
    (KeyCode::Numpad2, Key::Pad2),
    (KeyCode::Numpad3, Key::Pad3),
    (KeyCode::Numpad4, Key::Pad4),
    (KeyCode::Numpad5, Key::Pad5),
    (KeyCode::Numpad6, Key::Pad6),
    (KeyCode::Numpad7, Key::Pad7),
    (KeyCode::Numpad8, Key::Pad8),
    (KeyCode::Numpad9, Key::Pad9),
    (KeyCode::NumpadDivide, Key::PadDivide),
    (KeyCode::NumpadMultiply, Key::PadMultiply),
    (KeyCode::NumpadSubtract, Key::PadMinus),
    (KeyCode::NumpadAdd, Key::PadPlus),
    (KeyCode::NumpadEnter, Key::PadEnter),
    (KeyCode::NumpadDecimal, Key::PadDecimal),
    (KeyCode::BracketLeft, Key::LeftBracket),
    (KeyCode::BracketRight, Key::RightBracket),
    (KeyCode::Semicolon, Key::Semicolon),
    (KeyCode::Quote, Key::Apostrophe),
    (KeyCode::Backquote, Key::Backquote),
    (KeyCode::Comma, Key::Comma),
    (KeyCode::Period, Key::Period),
    (KeyCode::Slash, Key::Slash),
    (KeyCode::Backslash, Key::Backslash),
    (KeyCode::Minus, Key::Minus),
    (KeyCode::Equal, Key::Equal),
    (KeyCode::Enter, Key::Enter),
    (KeyCode::Space, Key::Space),
    (KeyCode::Backspace, Key::Backspace),
    (KeyCode::Tab, Key::Tab),
    (KeyCode::CapsLock, Key::CapsLock),
    (KeyCode::NumLock, Key::NumLock),
    (KeyCode::Escape, Key::Escape),
    (KeyCode::ScrollLock, Key::ScrollLock),
    (KeyCode::Insert, Key::Insert),
    (KeyCode::Delete, Key::Delete),
    (KeyCode::Home, Key::Home),
    (KeyCode::End, Key::End),
    (KeyCode::PageUp, Key::PageUp),
    (KeyCode::PageDown, Key::PageDown),
    (KeyCode::Pause, Key::Break),
    (KeyCode::ShiftLeft, Key::LeftShift),
    (KeyCode::ShiftRight, Key::RightShift),
    (KeyCode::AltLeft, Key::LeftAlt),
    (KeyCode::AltRight, Key::RightAlt),
    (KeyCode::ControlLeft, Key::LeftControl),
    (KeyCode::ControlRight, Key::RightControl),
    (KeyCode::SuperLeft, Key::LeftSuper),
    (KeyCode::SuperRight, Key::RightSuper),
    (KeyCode::ContextMenu, Key::App),
    (KeyCode::ArrowUp, Key::Up),
    (KeyCode::ArrowLeft, Key::Left),
    (KeyCode::ArrowDown, Key::Down),
    (KeyCode::ArrowRight, Key::Right),
    (KeyCode::F1, Key::F1),
    (KeyCode::F2, Key::F2),
    (KeyCode::F3, Key::F3),
    (KeyCode::F4, Key::F4),
    (KeyCode::F5, Key::F5),
    (KeyCode::F6, Key::F6),
    (KeyCode::F7, Key::F7),
    (KeyCode::F8, Key::F8),
    (KeyCode::F9, Key::F9),
    (KeyCode::F10, Key::F10),
    (KeyCode::F11, Key::F11),
    (KeyCode::F12, Key::F12),
];

/// The key at that position, if the engine has one for it.
pub fn key(code: KeyCode) -> Option<Key> {
    KEYS.iter()
        .find(|&&(candidate, _)| candidate == code)
        .map(|&(_, key)| key)
}

/// `MouseButton::Back`/`Forward` are `MOUSE_4`/`MOUSE_5`; the wheel's two fake
/// buttons are synthesized from `MouseWheel` instead, and `Other` buttons have
/// no `ButtonCode_t`.
pub fn mouse(button: winit::event::MouseButton) -> Option<MouseButton> {
    match button {
        winit::event::MouseButton::Left => Some(MouseButton::Left),
        winit::event::MouseButton::Right => Some(MouseButton::Right),
        winit::event::MouseButton::Middle => Some(MouseButton::Middle),
        winit::event::MouseButton::Back => Some(MouseButton::Mouse4),
        winit::event::MouseButton::Forward => Some(MouseButton::Mouse5),
        winit::event::MouseButton::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::input::Button;

    /// A [`Key`] with no row is a key that does nothing when pressed, and
    /// nothing else in the engine would report it: the binding would parse,
    /// the name would round-trip, and the key would simply be dead.
    #[test]
    fn every_key_has_exactly_one_position() {
        for button in Button::all() {
            let Button::Key(key) = button else { continue };
            let rows = KEYS.iter().filter(|&&(_, mapped)| mapped == key).count();
            assert_eq!(rows, 1, "{} maps from {rows} key codes", button.name());
        }
        assert_eq!(KEYS.len(), Key::COUNT);
    }

    #[test]
    fn positions_are_translated_not_labels() {
        // The AZERTY case: `KeyCode::KeyW` is the key *where W is on QWERTY*,
        // which is where Z is printed on a French keyboard. Binding by
        // position is what keeps WASD a square.
        assert_eq!(key(KeyCode::KeyW), Some(Key::W));
        assert_eq!(key(KeyCode::KeyA), Some(Key::A));
        assert_eq!(key(KeyCode::KeyS), Some(Key::S));
        assert_eq!(key(KeyCode::KeyD), Some(Key::D));
    }

    #[test]
    fn the_awkward_names_line_up() {
        assert_eq!(key(KeyCode::Quote), Some(Key::Apostrophe));
        assert_eq!(key(KeyCode::Pause), Some(Key::Break));
        assert_eq!(key(KeyCode::ContextMenu), Some(Key::App));
        assert_eq!(key(KeyCode::NumpadSubtract), Some(Key::PadMinus));
        assert_eq!(key(KeyCode::NumpadDecimal), Some(Key::PadDecimal));
        assert_eq!(key(KeyCode::SuperLeft), Some(Key::LeftSuper));
    }

    #[test]
    fn keys_the_engine_has_no_code_for_produce_nothing() {
        assert_eq!(key(KeyCode::F13), None);
        assert_eq!(key(KeyCode::IntlYen), None);
        assert_eq!(key(KeyCode::MediaPlayPause), None);
        assert_eq!(key(KeyCode::PrintScreen), None);
    }

    #[test]
    fn the_extra_mouse_buttons_are_back_and_forward() {
        use winit::event::MouseButton as Winit;
        assert_eq!(mouse(Winit::Back), Some(MouseButton::Mouse4));
        assert_eq!(mouse(Winit::Forward), Some(MouseButton::Mouse5));
        assert_eq!(mouse(Winit::Other(9)), None);
    }
}
