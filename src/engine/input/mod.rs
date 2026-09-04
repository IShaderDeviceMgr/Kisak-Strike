//! Input: what the platform reported, what is held, and where the view points.
//!
//! Replaces `inputsystem/` (the device layer), `engine/keys.cpp` and
//! `sys_mainwind.cpp`'s `DispatchInputEvent` (the dispatch layer), and stands
//! in for `game/client/in_*.cpp` (the movement layer) until `client/` exists.
//! `portdocs/ENGINE_INPUT.md` is the standing analysis.
//!
//! # Three layers, and where they live
//!
//! | Layer | Original | Here |
//! |---|---|---|
//! | Device | `inputsystem/` (10,649 lines) | `winit`, translated in [`window`](crate::engine::window) |
//! | Dispatch | `keys.cpp` + `DispatchInputEvent` | this module |
//! | Movement | `in_main.cpp`, `in_mouse.cpp` | [`view`]'s placeholder camera, until `client/` |
//!
//! **This module names no windowing type.** `window/` translates
//! `WindowEvent` into [`Event`] and pushes; everything here is `std` and
//! `glam`, and is tested without a window or a GPU — the same property
//! [`host`](crate::engine::host) has, for the same reason. It is also what
//! leaves room for `gilrs`, which is polled rather than pushed and is not a
//! `winit` concept (`portdocs/ENGINE_INPUT.md` §10).
//!
//! # Push between ticks, drain once per tick
//!
//! [`Input::push`] is `PostEvent`; [`Input::frame`] is
//! `DispatchAllStoredGameMessages`, and it runs at the top of
//! [`Engine::frame`](crate::engine::Engine::frame) *after*
//! [`Host::frame`](crate::engine::host::Host::frame) has agreed a frame is
//! happening. That split is not cosmetic. `winit` delivers events whether or
//! not a frame runs, and `FrameClock` refuses frames whenever `fps_max` says
//! one is early, so:
//!
//! - Events queue up between ticks and are dispatched in arrival order.
//! - **Mouse motion accumulates as a sum**, never as a last-value. Applying
//!   per event would make turn speed depend on event rate; keeping the last
//!   delta would discard motion on every refused frame. This is what
//!   `m_flAccumulatedMouseXMovement` was for, and under
//!   `ControlFlow::WaitUntil` pacing it matters more than it did for Valve.
//!
//! The consequence worth stating once: input is sampled at the *frame* rate,
//! not the event rate, so a lower `fps_max` is a higher input latency. That is
//! faithful to `CInput::AccumulateMouse`.

pub mod bind;
pub mod button;
pub mod view;

pub use bind::{Bindings, CommandSink};
pub use button::{Button, Key, MouseButton};
pub use view::MoveButtons;

/// The modifiers `toggleconsole` is swallowed under (`engine/keys.cpp:1172`).
const MODIFIERS: &[Key] = &[
    Key::LeftShift,
    Key::RightShift,
    Key::LeftControl,
    Key::RightControl,
    Key::LeftAlt,
    Key::RightAlt,
];

/// One thing the platform reported.
///
/// `InputEvent_t` (`public/inputsystem/InputEnums.h`) minus the
/// three-events-per-keypress it was built on. `CInputSystem::PollInputState`
/// posted `IE_ButtonPressed` (with a scan code *and* a virtual code),
/// `IE_KeyCodeTyped` and `IE_KeyTyped` for one key press, then spent a hundred
/// lines undoing SDL's double-reporting of the same fact. `winit`'s `KeyEvent`
/// carries all three in one struct, so the triple-post, the synthetic
/// backspace and the scancode tables are answered rather than ported
/// (`portdocs/ENGINE_INPUT.md` §4.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// `IE_ButtonPressed`. `repeat` is the OS's auto-repeat, passed through
    /// rather than filtered: a console wants repeat and a binding must not
    /// have it (`kbutton_t`'s `KeyDown` returns early on one,
    /// `in_main.cpp:434`), so the consumer decides.
    Pressed { button: Button, repeat: bool },
    /// `IE_ButtonReleased`.
    Released(Button),
    /// `IE_KeyTyped` — a character, not a key. Text entry only; nothing
    /// consumes it until `console/`.
    Text(char),
    /// Raw relative motion, in device units. **Look input, and only this.**
    ///
    /// `CursorMoved` deltas are clamped to the window and quantised to pixels,
    /// so a view driven from them stalls at the screen edge — the classic
    /// "cannot turn past 180 degrees" bug.
    MouseMotion { dx: f32, dy: f32 },
    /// Absolute cursor position in the window, in physical pixels. **UI only.**
    CursorMoved { x: f32, y: f32 },
    /// Wheel movement in notches, positive away from the user. The discrete
    /// [`MouseButton::WheelUp`]/[`MouseButton::WheelDown`] presses arrive
    /// separately, because both spellings are content.
    Wheel(f32),
    /// The window lost focus. Releases everything held — see [`Input::clear`].
    FocusLost,
    /// The window got focus back.
    ///
    /// Not in Valve's event set, which had no reason for it: this port needs
    /// it because X11 delivers raw motion from the device whether or not the
    /// window is focused, so [`Input`] has to know.
    FocusGained,
}

/// Everything the engine knows about the input devices.
///
/// One player. Valve's `PerUserInput_t` arrays and
/// `ACTIVE_SPLITSCREEN_PLAYER_GUARD` are not ported: the binding table was
/// global even in the original, so split-screen reduces to whether the
/// down-state and the view angles are one object or an array, which stays
/// cheap to defer as long as nothing bakes a player slot into [`Event`] or
/// [`Button`] (`portdocs/ENGINE_INPUT.md` §11.1).
pub struct Input {
    /// Posted since the last tick, in arrival order.
    queue: Vec<Event>,
    /// The last tick's surviving events. Reused, so a steady frame allocates
    /// nothing.
    tick: Vec<Event>,
    /// `CKeyInfo::m_bKeyDown`, one per button.
    down: [bool; Button::COUNT],
    /// Summed raw motion since the last [`frame`](Input::frame).
    mouse: (f32, f32),
    /// Whether the mouse is driving the view. `CInput::m_fMouseActive`, and
    /// the request `window/` turns into a cursor grab.
    mouse_look: bool,
    /// Whether the window has focus.
    focused: bool,
    /// Button to command text. Global even in the original — `s_KeyContext`
    /// held one table, not one per splitscreen player.
    bindings: Bindings,
    /// What the `+command`s a binding sends have left held.
    ///
    /// Client state living in `input/` for the same reason
    /// [`view`](crate::engine::input::view) does: there is nowhere else yet.
    /// **Moves to `client/` with [`view::FlyCamera`]**.
    move_buttons: MoveButtons,
}

impl Input {
    pub fn new() -> Input {
        Input {
            queue: Vec::new(),
            tick: Vec::new(),
            down: [false; Button::COUNT],
            mouse: (0.0, 0.0),
            // A game window that has just opened owns the mouse, as Source's
            // does. Escape gives it back; see `Engine::frame`.
            mouse_look: true,
            // Assumed, and corrected immediately: `window/` seeds this from
            // `Window::has_focus` the moment there is a window, because a
            // window the desktop never activated never sends a `Focused`
            // event to correct an assumption with.
            focused: true,
            bindings: Bindings::new(),
            move_buttons: MoveButtons::default(),
        }
    }

    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    pub fn bindings_mut(&mut self) -> &mut Bindings {
        &mut self.bindings
    }

    pub fn move_buttons(&self) -> &MoveButtons {
        &self.move_buttons
    }

    pub fn move_buttons_mut(&mut self) -> &mut MoveButtons {
        &mut self.move_buttons
    }

    /// Turns this tick's presses and releases into command text.
    ///
    /// `Key_Event`'s dispatch half (`engine/keys.cpp:1130`), called once per
    /// frame **after** [`frame`](Input::frame) and **before** the console runs,
    /// so that a key pressed this tick has its command executed in the same
    /// tick rather than the next one.
    ///
    /// Reads [`events`](Input::events), so the redundant-transition guard has
    /// already run: an auto-repeat press for a key that is already down never
    /// reaches here, which is what `kbutton_t::KeyDown`'s "repeating key" early
    /// return handles in the original.
    pub fn dispatch_bindings(&self, sink: &mut dyn CommandSink) {
        // `keys.cpp:1170` swallows `toggleconsole` under any modifier. Sampled
        // once for the tick rather than per event: the modifier state cannot
        // change within a tick, because a modifier press is itself an event in
        // this same list and the guard is about what is *held*.
        let modifier_down = MODIFIERS.iter().any(|&key| self.is_down(Button::Key(key)));

        for event in &self.tick {
            let (button, down) = match *event {
                Event::Pressed { button, .. } => (button, true),
                Event::Released(button) => (button, false),
                _ => continue,
            };
            self.bindings.dispatch(button, down, modifier_down, sink);
        }
    }

    /// Posts one event, between ticks. `CInputSystem::PostEvent`.
    ///
    /// Raw motion is **dropped rather than accumulated** unless the mouse is
    /// actually driving the view. It is meaningless without a grab, and X11's
    /// XI2 raw events arrive from the device even when the window is not
    /// focused, so alt-tabbing away and moving the mouse would otherwise spin
    /// the view. Dropping it at the door is also what keeps the accumulator
    /// from delivering one enormous delta on the frame the grab comes back —
    /// `CInput::ResetMouse`'s job (`in_mouse.cpp:342`).
    pub fn push(&mut self, event: Event) {
        match event {
            Event::MouseMotion { dx, dy } => {
                if !self.mouse_look || !self.focused {
                    return;
                }
                self.mouse.0 += dx;
                self.mouse.1 += dy;
            }
            Event::FocusLost => {
                self.focused = false;
                self.mouse = (0.0, 0.0);
            }
            Event::FocusGained => {
                self.focused = true;
                self.mouse = (0.0, 0.0);
            }
            _ => {}
        }
        self.queue.push(event);
    }

    /// Dispatches everything posted since the last tick, and returns the
    /// summed raw mouse motion.
    ///
    /// `DispatchAllStoredGameMessages` (`sys_mainwind.cpp:509`) and
    /// `GetAccumulatedMouseDeltasAndResetAccumulators` (`in_mouse.cpp:365`) in
    /// one call, because they have to happen together: this is the single
    /// point at which the accumulators reset, and a second one would silently
    /// halve the motion.
    ///
    /// Events that do not change the down-state are **dropped here**, not
    /// passed on — see [`Input::events`].
    pub fn frame(&mut self) -> (f32, f32) {
        // Swapped out rather than borrowed, so dispatch can touch `self`. The
        // emptied queue goes back afterwards with its allocation intact.
        let mut queue = std::mem::take(&mut self.queue);
        self.tick.clear();

        for event in queue.drain(..) {
            match event {
                Event::Pressed { button, .. } => {
                    if !self.transition(button, true) {
                        continue;
                    }
                }
                Event::Released(button) => {
                    if !self.transition(button, false) {
                        continue;
                    }
                }
                // `CInput::ClearStates` (`in_mouse.cpp:828`). Alt-tabbing with
                // `+forward` held and coming back to a player who has walked
                // into a wall for thirty seconds is the failure this prevents.
                Event::FocusLost => self.clear(),
                _ => {}
            }
            self.tick.push(event);
        }

        self.queue = queue;
        std::mem::replace(&mut self.mouse, (0.0, 0.0))
    }

    /// The redundant-transition guard: "don't handle key ups if the key's
    /// already up" (`keys.cpp:1284`).
    ///
    /// Load-bearing twice over. Valve needed it because several paths could
    /// report the same transition; this port needs it because `winit` emits
    /// **synthetic key events on focus change** (`is_synthetic: true`) to
    /// report keys that were already held. With the guard those are free; a
    /// press that arrives twice is one press, and a release for a key that is
    /// already up is nothing at all.
    ///
    /// Returns whether the event changed anything and should be dispatched.
    fn transition(&mut self, button: Button, down: bool) -> bool {
        if self.down[button.index()] == down {
            return false;
        }
        self.down[button.index()] = down;
        true
    }

    /// This tick's events, in arrival order, as [`frame`](Input::frame) left
    /// them.
    ///
    /// Valid until the next `frame`. Redundant transitions are already gone,
    /// so a consumer that turns a press into `+attack` cannot send it twice.
    pub fn events(&self) -> &[Event] {
        &self.tick
    }

    /// Whether a button is held.
    pub fn is_down(&self, button: Button) -> bool {
        self.down[button.index()]
    }

    /// Whether the mouse is driving the view.
    ///
    /// `window/` reads this every frame and reconciles the cursor grab with
    /// it; the grab is `mouse_look && focused`, because a captured cursor in a
    /// window the user has alt-tabbed away from is a trapped cursor.
    pub fn mouse_look(&self) -> bool {
        self.mouse_look
    }

    /// `CInput::ActivateMouse`/`DeactivateMouse` (`in_mouse.cpp:296`).
    ///
    /// Clears the accumulator on any change, so that neither the motion made
    /// while the cursor was free nor the jump on the way back reaches the
    /// view.
    pub fn set_mouse_look(&mut self, on: bool) {
        if self.mouse_look != on {
            self.mouse_look = on;
            self.mouse = (0.0, 0.0);
        }
    }

    /// Releases everything held. `CInput::ClearStates`.
    ///
    /// Deliberately does *not* touch [`mouse_look`](Input::mouse_look): losing
    /// focus suspends the grab, it does not decide that the game no longer
    /// wants the mouse.
    pub fn clear(&mut self) {
        self.down = [false; Button::COUNT];
        self.mouse = (0.0, 0.0);
        // The `+command`s a binding sent are held by the *command*, not by the
        // key, so clearing the down-state is not enough: without this, alt-tab
        // with `+forward` held leaves the camera walking forever, which is the
        // exact failure `CInput::ClearStates` exists to prevent.
        self.move_buttons.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> Button {
        Button::Key(key)
    }

    fn pressed(button: Button) -> Event {
        Event::Pressed {
            button,
            repeat: false,
        }
    }

    #[test]
    fn a_press_is_held_until_its_release() {
        let mut input = Input::new();
        assert!(!input.is_down(key(Key::W)));

        input.push(pressed(key(Key::W)));
        input.frame();
        assert!(input.is_down(key(Key::W)));

        input.push(Event::Released(key(Key::W)));
        input.frame();
        assert!(!input.is_down(key(Key::W)));
    }

    #[test]
    fn events_are_dispatched_in_arrival_order() {
        let mut input = Input::new();
        input.push(pressed(key(Key::A)));
        input.push(pressed(key(Key::B)));
        input.push(Event::Released(key(Key::A)));
        input.frame();

        assert_eq!(
            input.events(),
            [
                pressed(key(Key::A)),
                pressed(key(Key::B)),
                Event::Released(key(Key::A))
            ]
        );
    }

    /// `keys.cpp:1284`, and the reason `winit`'s synthetic focus-change key
    /// events cost nothing. Without it, a press reported twice sends
    /// `+attack` twice and the matching release only stops one of them.
    #[test]
    fn a_transition_that_changes_nothing_is_dropped() {
        let mut input = Input::new();
        input.push(pressed(key(Key::W)));
        input.push(pressed(key(Key::W)));
        input.push(Event::Released(key(Key::W)));
        input.push(Event::Released(key(Key::W)));
        input.frame();

        assert_eq!(
            input.events(),
            [pressed(key(Key::W)), Event::Released(key(Key::W))],
            "the repeats never reach a consumer"
        );
    }

    #[test]
    fn a_tick_with_no_events_leaves_no_events() {
        let mut input = Input::new();
        input.push(pressed(key(Key::W)));
        input.frame();
        input.frame();
        assert!(input.events().is_empty(), "the last tick's events are gone");
        assert!(input.is_down(key(Key::W)), "but the state survives");
    }

    /// The correctness trap of `portdocs/ENGINE_INPUT.md` §6.4: `fps_max`
    /// refuses frames, so several motion events arrive per tick. Summing is
    /// what makes turn speed independent of event rate.
    #[test]
    fn motion_accumulates_across_refused_frames() {
        let mut input = Input::new();
        for _ in 0..4 {
            input.push(Event::MouseMotion { dx: 3.0, dy: -1.0 });
        }
        assert_eq!(input.frame(), (12.0, -4.0));
        assert_eq!(input.frame(), (0.0, 0.0), "and the accumulator resets");
    }

    #[test]
    fn motion_is_dropped_while_the_mouse_is_not_driving_the_view() {
        let mut input = Input::new();
        input.set_mouse_look(false);
        input.push(Event::MouseMotion { dx: 50.0, dy: 50.0 });
        assert_eq!(input.frame(), (0.0, 0.0));

        // And re-capturing does not deliver the motion made in between.
        input.set_mouse_look(true);
        assert_eq!(input.frame(), (0.0, 0.0));
    }

    #[test]
    fn losing_focus_releases_everything_held() {
        let mut input = Input::new();
        input.push(pressed(key(Key::W)));
        input.push(pressed(Button::Mouse(MouseButton::Left)));
        input.frame();

        input.push(Event::FocusLost);
        input.frame();
        assert!(!input.is_down(key(Key::W)));
        assert!(!input.is_down(Button::Mouse(MouseButton::Left)));

        // The release that arrives after the window comes back is now a
        // redundant transition, and the guard eats it.
        input.push(Event::FocusGained);
        input.push(Event::Released(key(Key::W)));
        input.frame();
        assert_eq!(input.events(), [Event::FocusGained]);
    }

    #[test]
    fn motion_while_unfocused_never_reaches_the_view() {
        // X11 delivers XI2 raw motion from the device, not from the window.
        let mut input = Input::new();
        input.push(Event::FocusLost);
        input.push(Event::MouseMotion { dx: 400.0, dy: 0.0 });
        assert_eq!(input.frame(), (0.0, 0.0));
    }

    #[test]
    fn losing_focus_does_not_give_up_the_mouse() {
        let mut input = Input::new();
        input.push(Event::FocusLost);
        input.frame();
        assert!(
            input.mouse_look(),
            "the grab is suspended by `window/`, not surrendered"
        );
    }

    #[test]
    fn text_and_cursor_events_pass_through_untouched() {
        let mut input = Input::new();
        input.push(Event::Text('w'));
        input.push(Event::CursorMoved { x: 4.0, y: 8.0 });
        input.push(Event::Wheel(1.0));
        input.frame();
        assert_eq!(
            input.events(),
            [
                Event::Text('w'),
                Event::CursorMoved { x: 4.0, y: 8.0 },
                Event::Wheel(1.0)
            ]
        );
    }

    #[derive(Default)]
    struct Sink(Vec<String>);

    impl CommandSink for Sink {
        fn enqueue(&mut self, command: &str) {
            self.0.push(command.to_string());
        }
    }

    fn dispatched(input: &Input) -> Vec<String> {
        let mut sink = Sink::default();
        input.dispatch_bindings(&mut sink);
        sink.0
    }

    #[test]
    fn a_bound_press_and_release_become_commands() {
        let mut input = Input::new();
        let w = Button::Key(Key::W);
        input.bindings_mut().bind(w, "+forward");

        input.push(Event::Pressed {
            button: w,
            repeat: false,
        });
        input.frame();
        assert_eq!(dispatched(&input), [format!("+forward {}", w.index())]);

        input.push(Event::Released(w));
        input.frame();
        assert_eq!(dispatched(&input), [format!("-forward {}", w.index())]);
    }

    /// The redundant-transition guard already dropped it, so the `+command`
    /// cannot be sent twice — which is what `kbutton_t::KeyDown`'s "repeating
    /// key" early return handles in the original.
    #[test]
    fn auto_repeat_never_reaches_the_binding() {
        let mut input = Input::new();
        let w = Button::Key(Key::W);
        input.bindings_mut().bind(w, "+forward");

        input.push(Event::Pressed {
            button: w,
            repeat: false,
        });
        input.push(Event::Pressed {
            button: w,
            repeat: true,
        });
        input.frame();
        assert_eq!(dispatched(&input).len(), 1);
    }

    #[test]
    fn a_modifier_held_swallows_toggleconsole() {
        let mut input = Input::new();
        let backquote = Button::Key(Key::Backquote);
        input.bindings_mut().bind(backquote, "toggleconsole");

        input.push(Event::Pressed {
            button: Button::Key(Key::LeftShift),
            repeat: false,
        });
        input.push(Event::Pressed {
            button: backquote,
            repeat: false,
        });
        input.frame();
        assert!(dispatched(&input).is_empty());
    }

    /// Losing focus with `+forward` held must not leave the view walking: the
    /// command holds the button, so clearing the key down-state is not enough.
    #[test]
    fn focus_loss_releases_what_the_commands_are_holding() {
        let mut input = Input::new();
        input.move_buttons_mut().apply("forward", true, Some(3));
        assert!(input.move_buttons().forward.is_down());

        input.push(Event::FocusLost);
        input.frame();
        assert!(!input.move_buttons().forward.is_down());
    }
}
