//! The binding table, and the seam that turns a key press into command text.
//!
//! `engine/keys.cpp`'s `Key_SetBinding` (`:117`), the `bind`/`unbind`/
//! `unbindall` commands, and `Key_Event`'s dispatch (`:1130`) — which is the
//! part worth reading carefully, because the `+`/`-` convention is subtler than
//! it looks. `portdocs/ENGINE_INPUT.md` §8.3 is the design.
//!
//! `CKeyInfo`'s `m_pKeyBinding` was a `char*` per button in a global
//! `s_KeyContext`; here it is a `Vec` indexed by [`Button::index`], owned by
//! [`Input`](super::Input).
//!
//! # What is deliberately not here
//!
//! `Key_SetBinding`'s `GetSplitPlayerJoystickCode` remapping, `unbindalljoystick`
//! and `unbindallmousekeyboard` (splitscreen and controllers, §5 and §10), and
//! the guard at `keys.cpp:1139` that refuses every binding except
//! `toggleconsole` while the client is not connected — that one needs
//! `engineClient->IsConnected()`, so it arrives with `client/`.

use super::Button;

/// Where [`Bindings::dispatch`] sends the command text it builds.
///
/// `Cbuf_AddText`, as a trait, so that `input/` names no console type and
/// `console/` names no input type. The implementation lives in
/// `src/engine/mod.rs` for that reason: either module could write it, and
/// putting it in either one would create the dependency this exists to avoid.
///
/// The same move [`Level`](crate::engine::host::Level) and
/// [`CommandTarget`](crate::engine::console::CommandTarget) make.
pub trait CommandSink {
    /// Queues command text, as though it had been typed.
    fn enqueue(&mut self, command: &str);
}

/// Button to command text.
#[derive(Debug)]
pub struct Bindings {
    /// Indexed by [`Button::index`]. `None` is unbound; the empty string is
    /// not representable, which is what `Key_SetBinding( b, "" )` meant.
    table: Vec<Option<Box<str>>>,
}

impl Default for Bindings {
    fn default() -> Bindings {
        Bindings::new()
    }
}

impl Bindings {
    pub fn new() -> Bindings {
        Bindings {
            table: vec![None; Button::COUNT],
        }
    }

    /// `Key_SetBinding` (`keys.cpp:117`).
    ///
    /// **Escape cannot be rebound**: `bind ESCAPE <anything>` stores
    /// `cancelselect` regardless (`keys.cpp:310`). That is not a quirk to drop
    /// — it is what guarantees there is always a way out of a menu, and this
    /// port has the same need for a different reason: Escape is currently the
    /// only way to release the captured cursor.
    pub fn bind(&mut self, button: Button, command: &str) {
        let command = match button {
            Button::Key(super::Key::Escape) => "cancelselect",
            _ => command,
        };
        self.table[button.index()] = match command.is_empty() {
            true => None,
            false => Some(command.into()),
        };
    }

    /// `Key_SetBinding( b, "" )`.
    ///
    /// Escape is refused (`keys.cpp:183`). Returns whether anything changed,
    /// so the command can say "Can't unbind ESCAPE key".
    pub fn unbind(&mut self, button: Button) -> bool {
        if button == Button::Key(super::Key::Escape) {
            return false;
        }
        self.table[button.index()] = None;
        true
    }

    /// `unbindall` (`keys.cpp:191`).
    ///
    /// **Escape and the backquote survive**, exactly as Valve's loop skips
    /// them: `config_default.cfg` opens with `unbindall`, so without the two
    /// exceptions a user who exec'd it would lose the console key and the
    /// menu key at once and have no way to get either back.
    pub fn unbind_all(&mut self) {
        for button in Button::all() {
            if matches!(
                button,
                Button::Key(super::Key::Escape) | Button::Key(super::Key::Backquote)
            ) {
                continue;
            }
            self.table[button.index()] = None;
        }
    }

    pub fn get(&self, button: Button) -> Option<&str> {
        self.table[button.index()].as_deref()
    }

    /// Every bound button, in index order. `key_listboundkeys`.
    pub fn iter(&self) -> impl Iterator<Item = (Button, &str)> {
        self.table
            .iter()
            .enumerate()
            .filter_map(|(index, command)| Some((Button::from_index(index)?, command.as_deref()?)))
    }

    /// `Key_CountBindings` (`engine/keys.cpp:510`).
    ///
    /// Read before writing a config: Valve refuses to write one when this is
    /// `<= 1` (`host.cpp:1603`), on the grounds that a session which somehow
    /// bound nothing must not be allowed to persist that over a real config.
    pub fn count(&self) -> usize {
        self.table.iter().filter(|b| b.is_some()).count()
    }

    /// `Key_WriteBindings` (`engine/keys.cpp:533`): one `bind "<key>"
    /// "<command>"` line per bound button, in button order.
    ///
    /// The names are the fixed external format — `s_pButtonCodeName` — so a
    /// name that does not round-trip is a binding that vanishes from the user's
    /// config the next time it is read back.
    pub fn write(&self, out: &mut String) {
        for (button, command) in self.iter() {
            out.push_str(&format!("bind \"{}\" \"{}\"\n", button.name(), command));
        }
    }

    /// Whether this button must reach the game whatever the UI wants.
    ///
    /// `Key_Event` bypasses the whole VGui chain for a `KEY_BACKQUOTE` press
    /// (`engine/keys.cpp:1319`), and it has to: otherwise the key that opens
    /// the console cannot close it, and it types a backquote into the entry on
    /// the way. Generalised from "the backquote" to "whatever is bound to
    /// `toggleconsole`", which is the same rule with the key no longer
    /// hard-coded — `bind p toggleconsole` then behaves like the shipped
    /// binding rather than like a key that opens a console it cannot close.
    ///
    /// Read by `window/`, which is where the UI's answer is decided; the
    /// engine reaches it through
    /// [`Engine::ui_bypasses`](crate::engine::Engine::ui_bypasses).
    pub fn bypasses_ui(&self, button: Button) -> bool {
        self.get(button)
            .is_some_and(|command| command.eq_ignore_ascii_case("toggleconsole"))
    }

    /// Every button bound to `command`, compared case-insensitively.
    /// `key_findbinding`.
    pub fn find(&self, command: &str) -> impl Iterator<Item = Button> + '_ {
        let wanted = command.to_ascii_lowercase();
        self.iter()
            .filter(move |(_, bound)| bound.to_ascii_lowercase().contains(&wanted))
            .map(|(button, _)| button)
    }

    /// Turns one press or release into command text. `Key_Event`'s tail
    /// (`keys.cpp:1130`).
    ///
    /// The `+`/`-` convention, which is the whole of this function and is
    /// asymmetric in a way that matters:
    ///
    /// - A binding starting with `+` sends `+forward <index>` on press and
    ///   `-forward <index>` on release.
    /// - Any other binding sends its text **on press only**. `bind F5 jpeg`
    ///   must not run `jpeg` twice.
    ///
    /// **The index argument is not decoration.** It is what
    /// [`KButton`](super::view::KButton)'s two-holder set matches on, so that
    /// releasing one of two keys bound to `+forward` does not stop the
    /// movement while the other is still held. Valve's comment says it
    /// outright: "*Button commands include the kenum as a parameter, so
    /// multiple downs can be matched with ups*".
    ///
    /// `modifier_down` is for the one special case at `keys.cpp:1170`:
    /// `toggleconsole` is **swallowed** while a shift, control or alt is held,
    /// so that a chord passing through the console key does not open it.
    ///
    /// Returns false when nothing was sent.
    pub fn dispatch(
        &self,
        button: Button,
        down: bool,
        modifier_down: bool,
        sink: &mut dyn CommandSink,
    ) -> bool {
        let Some(binding) = self.get(button) else {
            return false;
        };

        // Valve passes `ButtonCode_t`; this passes our own index, which is a
        // different numbering and does not need to agree with Valve's. It is
        // only ever matched against itself, never written to content.
        let index = button.index();

        if let Some(command) = binding.strip_prefix('+') {
            let sign = if down { '+' } else { '-' };
            sink.enqueue(&format!("{sign}{command} {index}"));
            return true;
        }

        // A plain binding fires on the way down only.
        if !down {
            return false;
        }

        if binding.eq_ignore_ascii_case("toggleconsole") && modifier_down {
            return false;
        }

        sink.enqueue(binding);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::Key;
    use super::*;

    #[derive(Default)]
    struct Recorder(Vec<String>);

    impl CommandSink for Recorder {
        fn enqueue(&mut self, command: &str) {
            self.0.push(command.to_string());
        }
    }

    fn send(bindings: &Bindings, button: Button, down: bool) -> Vec<String> {
        let mut sink = Recorder::default();
        bindings.dispatch(button, down, false, &mut sink);
        sink.0
    }

    #[test]
    fn a_plus_binding_sends_both_edges_with_the_button_index() {
        let mut bindings = Bindings::new();
        let w = Button::Key(Key::W);
        bindings.bind(w, "+forward");

        assert_eq!(
            send(&bindings, w, true),
            [format!("+forward {}", w.index())]
        );
        assert_eq!(
            send(&bindings, w, false),
            [format!("-forward {}", w.index())]
        );
    }

    #[test]
    fn a_plain_binding_fires_on_the_way_down_only() {
        let mut bindings = Bindings::new();
        let f5 = Button::Key(Key::F5);
        bindings.bind(f5, "jpeg");

        assert_eq!(send(&bindings, f5, true), ["jpeg"]);
        assert!(
            send(&bindings, f5, false).is_empty(),
            "otherwise `bind F5 jpeg` takes two screenshots"
        );
    }

    #[test]
    fn an_unbound_button_sends_nothing() {
        let bindings = Bindings::new();
        assert!(send(&bindings, Button::Key(Key::J), true).is_empty());
    }

    #[test]
    fn toggleconsole_is_swallowed_under_a_modifier() {
        let mut bindings = Bindings::new();
        let key = Button::Key(Key::Backquote);
        bindings.bind(key, "toggleconsole");

        let mut sink = Recorder::default();
        assert!(bindings.dispatch(key, true, false, &mut sink));
        assert_eq!(sink.0, ["toggleconsole"]);

        let mut sink = Recorder::default();
        assert!(!bindings.dispatch(key, true, true, &mut sink));
        assert!(sink.0.is_empty(), "a chord must not open the console");
    }

    #[test]
    fn escape_always_binds_to_cancelselect() {
        let mut bindings = Bindings::new();
        let escape = Button::Key(Key::Escape);
        bindings.bind(escape, "quit");
        assert_eq!(
            bindings.get(escape),
            Some("cancelselect"),
            "there must always be a way out of a menu"
        );
    }

    #[test]
    fn escape_cannot_be_unbound_and_unbindall_spares_the_console_key() {
        let mut bindings = Bindings::new();
        let escape = Button::Key(Key::Escape);
        let backquote = Button::Key(Key::Backquote);
        let w = Button::Key(Key::W);

        bindings.bind(escape, "cancelselect");
        bindings.bind(backquote, "toggleconsole");
        bindings.bind(w, "+forward");

        assert!(!bindings.unbind(escape));
        assert_eq!(bindings.get(escape), Some("cancelselect"));

        // `config_default.cfg` opens with `unbindall`.
        bindings.unbind_all();
        assert_eq!(bindings.get(w), None);
        assert_eq!(bindings.get(escape), Some("cancelselect"));
        assert_eq!(bindings.get(backquote), Some("toggleconsole"));
    }

    #[test]
    fn binding_the_empty_string_unbinds() {
        let mut bindings = Bindings::new();
        let w = Button::Key(Key::W);
        bindings.bind(w, "+forward");
        bindings.bind(w, "");
        assert_eq!(bindings.get(w), None);
    }

    #[test]
    fn iter_and_find_see_what_was_bound() {
        let mut bindings = Bindings::new();
        bindings.bind(Button::Key(Key::W), "+forward");
        bindings.bind(Button::Key(Key::S), "+back");

        assert_eq!(bindings.iter().count(), 2);
        let found: Vec<Button> = bindings.find("+FORWARD").collect();
        assert_eq!(found, [Button::Key(Key::W)], "matching is case-insensitive");
    }

    #[test]
    fn the_written_config_reads_back_as_the_same_table() {
        let mut bindings = Bindings::new();
        bindings.bind(Button::Key(Key::W), "+forward");
        bindings.bind(Button::Mouse(super::super::MouseButton::Left), "+attack");
        bindings.bind(Button::Key(Key::F6), "save quick");
        assert_eq!(bindings.count(), 3);

        let mut out = String::new();
        bindings.write(&mut out);

        // The exact shape `config.cfg` has, and the shipped `exec` has to be
        // able to read it back.
        assert!(out.contains("bind \"w\" \"+forward\"\n"), "{out}");
        assert!(out.contains("bind \"MOUSE1\" \"+attack\"\n"), "{out}");
        assert!(
            out.contains("bind \"F6\" \"save quick\"\n"),
            "a multi-word command survives its quotes: {out}"
        );
        assert_eq!(out.lines().count(), 3);
    }
}
