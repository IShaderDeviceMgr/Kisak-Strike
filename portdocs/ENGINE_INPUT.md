# Porting input → `src/engine/input/`

**Status: stages 1 and 2 of §9's five are done** — translation, button state, mouse
capture, mouse look and a free-fly camera. **The API they landed is
`rustdocs/ENGINE.md`'s `src/engine/input/` section; read that to *use* the module, and
this to understand why it is shaped that way.** Stage 3 (bindings) wants `console/`,
stage 4 (UI precedence) wants `egui`, stage 5 (controllers) wants `gilrs`; §9 and §10
still stand as written for all three.

Written against the current architecture (single crate, no FFI, `winit`/`wgpu`); nothing
here assumes the old FFI-bridged model.

This is `portdocs/ENGINE.md` §7.3's remaining half, and it is the doc for three things at
once, because in the original tree input is spread across three modules that only make
sense together:

| Layer | Original | Job |
|---|---|---|
| Device | `inputsystem/` (10,649) | platform events → `InputEvent_t` |
| Dispatch | `engine/keys.cpp` (1,392) + `sys_mainwind.cpp`'s `DispatchInputEvent` | who gets the event; bindings → commands |
| Movement | `game/client/in_*.cpp` (7,402) | button state → view angles and `CUserCmd` |

Valve's top-level `inputsystem/` module gets no portdoc of its own; this is it. Naming
follows `ENGINE.md` §10's `ENGINE_<SUB>.md` convention for engine submodules.

Unqualified `§n` references are to sections of *this* document; references to other
docs are named (`ENGINE.md` §7.3).

**Read `PORTING.md` first**, then `ENGINE.md` §6 (the control-flow inversion, which this
completes) and `rustdocs/ENGINE.md` (the frame, which this hooks into).

---

## 0. Headline decisions

1. **Input becomes its own module, `src/engine/input/` — a fourteenth module.** This
   revises `ENGINE.md` §1, which put input translation in `window/` and `keys.cpp` in
   `console/`. Reasons in §8.1; the short version is that `window/` must stay the `winit`
   boundary and a controller backend is not a `winit` concept, and that `keys.cpp` was
   filed under `console/` by file-adjacency rather than by behavior.
2. **`window/` keeps only translation.** `WindowEvent` → `input::Event`, pushed into a
   queue. No state, no bindings, no policy. That is one `match` arm per event.
3. **Sample once per tick, accumulate between ticks.** The queue is drained at the top of
   `Engine::frame`, which is `DispatchAllStoredGameMessages`'s place in the loop. This is
   not cosmetic — see §6.4, the sharpest correctness trap in the module.
4. **Bindings are a trait seam, like `host::Level`.** `input/` produces command strings
   and hands them to a `CommandSink`; `console/` implements it later. That is what lets
   input land *before* `console/`, which is the order `PORTING.md` asks for.
5. **Controllers are stage 5 and use `gilrs`.** Deliberately not in the first landing —
   see §10. `gilrs` replaces the device layer only; `in_joystick.cpp`'s response curves
   are client-layer content tuning and come with the client.
6. **Bind by physical key, display by logical.** A deliberate divergence from Valve,
   recorded in §7.

---

## 1. Scope: three layers, and only two of them have a home yet

`src/engine/input/` is the device and dispatch layers. The movement layer needs
`CUserCmd`, a player entity and prediction — i.e. `client/` (`ENGINE.md` §7.5) and the
game DLL — and
none of that exists.

That leaves a gap that has to be filled with something, because the deliverable that makes
this port worth doing is *moving the camera*. The answer is a free-fly camera: the engine
already owns `Engine::camera` (`src/engine/mod.rs`), already has the placeholder
`TURN_RATE` turntable to delete, and a noclip fly-through is exactly `in_main.cpp`'s
angle math minus the parts that need a player. Stage 2 does that and no more.

**Do not port `CUserCmd`, `kbutton_t`'s two-key `down[2]` array, `CreateMove`,
prediction or `ScaleMovements` now.** They are correct and they are the right design, but
they are `client/`'s, and building them against a camera instead of a player would bake in
the wrong consumer.

---

## 2. Inventory

Line counts are `wc -l` against the tree at time of writing.

### `inputsystem/` — 10,649

| File | Lines | Disposition |
|---|---|---|
| `inputsystem.cpp` | 2,499 | **Dissolves.** The POSIX poll path (`PollInputState_Linux`/`_OSX`, `:846`) is a `CCocoaEvent` drain full of SDL bug workarounds — see §4.1. `winit` delivers what it was reconstructing. |
| `xcontroller.cpp` | 2,236 | **Deleted.** X360 XInput. Out of scope permanently. |
| `key_translation.cpp` | 1,339 | **Name tables survive, translation does not.** `s_pButtonCodeName` (`:357`) is external content (§7). The Win32/Cocoa/SDL scancode maps are what `winit` already did. |
| `movecontroller_ps3.cpp` + `.h` | 1,131 | **Deleted.** PS3 Move. |
| `joystick_osx.cpp` | 1,025 | **Deleted** — replaced by `gilrs` (§10). |
| `inputsystem.h` | 629 | Interface + state; dissolves. |
| `joystick_linux.cpp` | 566 | **Deleted** — `gilrs`. |
| `joystick.cpp` | 368 | **Deleted** — `gilrs`. Contains `joy_wingmanwarrior_centerhack`, a workaround for a 1990s Logitech stick, which is a fair summary of this file's value today. |
| `inputstacksystem.cpp` | 287 | **Deleted** — §5. |
| `steamcontroller.cpp` | 275 | **Deferred** with Steam integration (`PORTING.md` open question 5). |
| `novint.cpp` | 137 | **Deleted.** Novint Falcon haptic device; discontinued hardware. |
| `posix_stubs.h` | 118 | Dissolves. |
| `key_translation.h`, `Xbox/xbox.def` | 42 | Dissolves / deleted. |

**~5,900 lines are deleted purely because the hardware is out of scope** (X360, PS3,
Novint), before any porting judgment is applied.

### `public/inputsystem/` — 1,033

| File | Lines | Disposition |
|---|---|---|
| `ButtonCode.h` | 462 | **The knowledge survives, the encoding does not** (§4.2). |
| `iinputsystem.h` | 269 | **Deleted.** A 60-method `IAppSystem`; §5. |
| `InputEnums.h` | 182 | `InputEvent_t` and `InputEventType_t` become a Rust enum (§4.1). The Steam Controller half is deferred. |
| `iinputstacksystem.h` | 76 | **Deleted** — §5. |
| `AnalogCode.h` | 44 | `MOUSE_X`/`MOUSE_Y`/`MOUSE_WHEEL` survive as named fields, not codes. |

### `engine/` — ~1,580

| File | Lines | Disposition |
|---|---|---|
| `keys.cpp` | 1,392 | **The heart of the port.** Binding table, `+`/`-` command convention, the key-up latch (§4.3), trap mode. Roughly half is split-screen and Scaleform/RocketUI plumbing that goes. |
| `keys.h` | 38 | Becomes `input/`'s public surface. |
| `sys_mainwind.cpp` `DispatchInputEvent` | ~150 of 2,744 | The UI precedence chain (`:399`); collapses into egui's one answer (§8.3). |

### `game/client/in_*.cpp` — 7,402 (all deferred to `client/`)

| File | Lines | Disposition |
|---|---|---|
| `in_main.cpp` | 2,102 | `kbutton_t`, `KeyState`'s fractional model (§4.4), `AdjustAngles`, `CreateMove`. **Stage 2 borrows the angle math only.** |
| `in_joystick.cpp` | 2,016 | Response curves, deadzones, auto-aim dampening. **Client-layer, content-tuned; stage 5+ at the earliest** (§10). |
| `in_camera.cpp` | 1,079 | Third-person orbit camera. Portal 2 is first-person; effectively out of scope. |
| `in_mouse.cpp` | 843 | Accumulate/scale/apply (§4.5). **Stage 2 borrows the accumulate and apply halves.** |
| `in_forcefeedback.cpp` | 833 | **Deleted.** X360 rumble. Revisit via `gilrs`'s `ff` feature if rumble is ever wanted. |
| `in_steamcontroller.cpp` | 282 | Deferred with Steam. |
| `in_trackir.cpp` | 226 | **Deleted.** TrackIR head tracking. |

**Grand total in scope across the three layers: ~20,500 lines**, of which the first
landing (stages 1–2) should be well under 1,500 lines of Rust.

---

## 3. Dependency graph

**Inbound** — who needs `input/`:

- `window/` pushes events into it. This is the only *upward* call and it is one method.
- `engine/mod.rs` drains it once per tick and feeds the camera (stage 2).
- `console/` (`ENGINE.md` §7.4) will own `bind`/`unbind` as console commands and implement
  `CommandSink` (stage 3).
- The egui layer will get first refusal on every event (stage 4).
- `client/` (`ENGINE.md` §7.5) will consume button state to build `CUserCmd` (not
  scheduled).

**Outbound** — what `input/` needs:

- `std` and `glam` for stages 1–2. **Not `winit`.** See §8.1; the translation happens in
  `window/`, so `input/` names no windowing types and is testable without a window.
- `gilrs` at stage 5, and it is the only reason `input/` would gain a dependency.

That `input/` can be built and tested with no GPU and no window is the same property
`host/` has, and for the same reason: the platform lives on the other side of a seam.

---

## 4. The architecture you need in your head

### 4.1 The path today, and why almost all of it evaporates

`ENGINE.md` §6 has the full chain. The part worth reading in source before deleting it is
`CInputSystem::PollInputState_Linux`/`_OSX` (`inputsystem/inputsystem.cpp:846`), because
it shows what the layer was actually *for*. One key-down produces three posted events —
`IE_ButtonPressed` carrying a scan code and a virtual code, `IE_KeyCodeTyped`, and
`IE_KeyTyped` carrying a Unicode character — and the code is interleaved with comments
like:

> For SDL, hitting spacebar causes a SDL_KEYDOWN event, then SDL_TEXTINPUT with
> `event.text.text[0] = ' '`, and then we get here and wind up sending two events […]
> This will confuse `Button::OnKeyCodePressed()`

plus a Linux/OSX-only synthetic backspace event so Scaleform would see it, plus a
`CSTRIKE15` hack remapping Cmd+A/C/V/X to their Ctrl forms.

`winit` hands you all three facts in one struct:

```rust
KeyEvent { physical_key, logical_key, text: Option<SmolStr>, state, repeat }
```

So the triple-post, the SDL double-event workaround, the synthetic backspace and the
scancode tables are not ported — they are answered. **This is the single largest deletion
in the module, and it is why the device layer is not a stage of its own.**

### 4.2 `ButtonCode_t`: one flat enum for every binary input

`public/inputsystem/ButtonCode.h` is one integer space covering keyboard, mouse, four
joysticks (buttons, POV hats, and *axes-as-buttons*) and sixteen Steam Controllers, laid
out as `KEY_FIRST..KEY_LAST`, `MOUSE_FIRST..MOUSE_LAST`, `JOYSTICK_FIRST..`, with macros
(`JOYSTICK_BUTTON( joy, button )`) doing the index arithmetic and inline helpers
(`GetJoystickForCode`, `ButtonCodeToJoystickButtonCode`) doing the inverse.

**What to keep:** that every binary input is one flat, densely-indexed space. It is what
lets the binding table be an array and the down-state be a bitset, and it is why a
controller button can be bound to `+forward` with no special case anywhere.

**What to discard:** the arithmetic-in-macros encoding, and the fact that
`JOYSTICK_AXIS_BUTTON` exists at all — an analog axis synthesized into a pair of fake
buttons is a 1998 workaround for a binding system that could not express axes. `gilrs`
reports axes as axes.

The Rust shape is a real enum with a dense index (§8.2), which gets both properties.

Sizes worth knowing: `KEY_COUNT` is 107 — `KEY_NONE`, 103 real keys, and the three
`KEY_*TOGGLE` pseudo-keys — `MOUSE_COUNT` is 7 (five buttons plus
`MOUSE_WHEEL_UP`/`_DOWN` as fake buttons — **keep those two**, because `bind MWHEELUP
+jump` is real content), and analog values are scaled to `MAX_BUTTONSAMPLE` = 32768.

### 4.3 The key-up latch — the load-bearing algorithm

`FilterKey` (`engine/keys.cpp:1189`) is the one piece of `keys.cpp` that must survive
intact, and it is easy to miss because it looks like plumbing:

- When a target (VGui, GameUI, the client, the engine) **consumes a key-down**, the code
  records *which* target consumed it: `m_pKeyInfo[code].m_nKeyUpTarget = target`.
- The matching **key-up is delivered only to that target**, and is treated as consumed
  regardless.
- Comment in the source, worth reproducing: *"It is illegal to trap up key events. The
  system will do it for us."*

**Why it matters:** `bind mouse1 +attack`. Press mouse1 in game (the engine consumes it,
sends `+attack`), then open the console *before* releasing. Without the latch the console
eats the key-up, `-attack` never runs, and the player fires forever. Every "stuck key"
bug in a Source-like engine is this invariant being violated.

The same file has the companion guard at `keys.cpp:1284` — *"Don't handle key ups if the
key's already up"* — which rejects a transition that does not change state. That guard
becomes load-bearing for a second reason under `winit`: several backends emit **synthetic
key events on focus change** (`WindowEvent::KeyboardInput { is_synthetic: true }`) to
report keys already held. With the guard, those are free. Without it, they double-count.

### 4.4 `kbutton_t` and the fractional `KeyState`

`in_main.cpp:424`/`:460`/`:813`. A button is not a bool: it is `state` bits for
`down | impulse-down | impulse-up`, plus `down[2]`, the codes of up to two keys currently
holding it. `KeyState` (`:813`) converts that to a **fraction of the frame the button was
held**: 1.0 held throughout, 0.5 pressed this frame, 0.25 pressed *and* released within
one frame, 0.75 released and re-pressed. The impulse bits are cleared on read.

This is genuinely good design and the reason a 30 Hz frame does not swallow a fast tap. It
is also unambiguously **client-layer** — it feeds `ComputeForwardMove` and `CUserCmd` —
so it is documented here and deliberately not ported yet. `down[2]` is why two keys bound
to `+forward` do not cancel each other when one is released; it is also why the `+`/`-`
commands carry the button index as an argument (§8.3).

### 4.5 The mouse: accumulate, scale, apply

Three separate steps in `in_mouse.cpp`, and the separation is the point:

1. **Accumulate** (`AccumulateMouse`, `:614`; `GetAccumulatedMouseDeltasAndResetAccumulators`,
   `:365`). Deltas add up between samples and are reset on read. Under `m_rawinput 1`
   (the default) the source is the input system's raw accumulator; otherwise it is
   cursor-position-minus-window-centre followed by a warp back to centre (`ResetMouse`,
   `:342`).
2. **Scale** (`ScaleMouse`, `:412`): `sensitivity` (default 2.5), then optionally one of
   four `m_customaccel` curves.
3. **Apply** (`ApplyMouse`, `:470`): `viewangles[YAW] -= m_yaw * mouse_x`,
   `viewangles[PITCH] += m_pitch * mouse_y` (both default 0.022), then clamp pitch to
   `cl_pitchdown` / `-cl_pitchup` (both default 89).

Constants to carry across: `sensitivity` 2.5, `m_yaw` 0.022, `m_pitch` 0.022,
`cl_pitchdown` 89, `cl_pitchup` 89, `cl_yawspeed` 210, `cl_pitchspeed` 225,
`cl_anglespeedkey` 0.67 (`in_main.cpp:46-50`).

**Portal 2 specific, do not delete on sight:** `ApplyMouse` has a `#if defined PORTAL`
branch guarded by `cl_mouselook_roll_compensation` (default 1) that rotates the mouse
delta by the inverse of the current view *roll* before applying it, so that "mouse left"
stays "screen left" while the player is rolled — which happens constantly in Portal 2
(reorientation gels, portal transitions through non-vertical surfaces). It is a
quaternion round-trip in the original. It cannot be exercised until something rolls the
view, so it is stage 2+n, but it is **in scope for the target game** and belongs in the
port's memory now rather than being rediscovered as a bug report.

### 4.6 Where view angles live

`CInput::AdjustAngles` (`in_main.cpp:1006`) reads and writes them through
`engine->GetViewAngles`/`SetViewAngles`, and those resolve to
`GetLocalClient().viewangles` (`engine/cdll_engine_int.cpp:1050-1058`) — i.e. the angles
live in `CClientState`, which is `ENGINE.md` §7.5's `client/`, and the *client DLL* only
mutates them.

So: **the engine owning view angles is faithful, and `input/` owning them is a temporary
wart** with a known end condition. See §11.

---

## 5. What is deleted, and why

- **`IInputSystem` (269 lines, ~60 methods).** An `IAppSystem` with cursor icon loading,
  IME window management, mouse capture, rumble, motion-controller orientation
  quaternions, `SleepUntilInput`, virtual-key↔button-code conversion and platform input
  device enumeration. Under `PORTING.md`'s one-crate rule there is no interface to
  register, and most of the methods address problems `winit` does not have.
- **`IInputStackSystem` (`inputsystem/inputstacksystem.cpp`, 287 lines).** A
  priority stack of input contexts. Valve's own header comment is the argument for
  deleting it: *"For Source1, it would be a huge change to move all input (like the code
  in engine/keys.cpp for example) to go through this interface. Therefore, I'm going to
  stick with only dealing with cursor control."* It is a stack that manages exactly one
  thing — who owns the cursor — because the real dispatch chain was somewhere else. With
  egui there is one UI, so it is a boolean.
- **The IME event family** (`IE_IMESetWindow`, `IE_IMEStartComposition`,
  `IE_IMEShowCandidates`, …, `InputEnums.h:88-101`). Windows-only vgui text entry.
  `WindowEvent::Ime` exists if a console ever needs it.
- **Cursor icon management** (`InputStandardCursor_t`, `LoadCursorFromFile`,
  `ResetCursorIcon`). `winit`'s `Window::set_cursor` covers the standard set; custom
  cursor files are an egui concern if they return at all.
- **Split-screen plumbing** — `ACTIVE_SPLITSCREEN_PLAYER_GUARD`, `in_forceuser`,
  `PerUserInput_t` arrays, `GetSplitPlayerJoystickCode`. See §11 for the open question;
  the *mechanism* is deleted either way.
- **All out-of-scope hardware**: X360 (`xcontroller.cpp`, `in_forcefeedback.cpp`), PS3
  (`movecontroller_ps3.cpp`), Novint Falcon, TrackIR.
- **`Key_StartTrapMode`/`Key_CheckDoneTrapping`** (`keys.cpp:903-937`) — "press a key to
  bind it" for the options UI. Not deleted on principle, just not needed until there is
  an options UI; it is ~35 lines and trivially re-added.

---

## 6. The `winit` mapping, concretely

### 6.1 Events

`window/`'s `window_event` currently drops everything it does not recognise
(`src/engine/window/mod.rs`, the `_ => {}` arm that says "Input arrives here"). That arm
becomes:

| `winit` | → | `input::Event` | Notes |
|---|---|---|---|
| `WindowEvent::KeyboardInput { event, .. }` | | `Pressed`/`Released` + `Text` | `physical_key` selects the `Button`; `event.text` yields `Text`; `event.repeat` is passed through, not swallowed |
| `WindowEvent::MouseInput { state, button, .. }` | | `Pressed`/`Released` | `MouseButton::Back`/`Forward` → `MOUSE_4`/`MOUSE_5` |
| `WindowEvent::MouseWheel { delta, .. }` | | `Wheel` **and** `Pressed`+`Released` | The fake `MWHEELUP`/`MWHEELDOWN` buttons are content (§4.2); emit both. `LineDelta` and `PixelDelta` need reconciling |
| `WindowEvent::CursorMoved { position, .. }` | | `CursorMoved` | UI only — **never** view look; see §6.3 |
| `WindowEvent::Focused(false)` | | `FocusLost` | Must clear all held buttons; §6.5 |
| `WindowEvent::ModifiersChanged` | | — | Redundant: modifiers are ordinary `Button`s here, as they were `KEY_LSHIFT` etc. for Valve |
| `DeviceEvent::MouseMotion { delta }` | | `MouseMotion` | Raw look input, accumulated. Needs `ApplicationHandler::device_event`, which `GameWindow` does not implement yet |

### 6.2 Mouse capture is not portable, and this will cost a day

`winit 0.30.13`'s two grab modes are each unimplemented on one of the two platforms this
project supports (verified in `winit-0.30.13/src/window.rs:1687`):

- `CursorGrabMode::Locked` — **"X11: Not implemented. Always returns
  `ExternalError::NotSupported`."**
- `CursorGrabMode::Confined` — **"macOS: Not implemented. Always returns
  `ExternalError::NotSupported`."**

Neither mode works on both. `PORTING.md`'s supported set is Linux primary, macOS second,
and Linux means both Wayland (where `Locked` works) and X11 (where it does not).

**The fix is Valve's own fallback, and it is why `ResetMouse` exists:** try `Locked`; on
`NotSupported`, use `Confined` plus `Window::set_cursor_position` warping to the window
centre each frame, which is exactly `in_mouse.cpp:342`'s warp-to-centre path. Hide the
cursor with `set_cursor_visible(false)` in both cases — neither grab mode promises to.

Write this as one `enum Capture { Locked, Warped { centre: (f64, f64) } }` decided once at
grab time, not as a `cfg!` — the failing case is a runtime property of the session
(X11-vs-Wayland is not a compile-time fact), and the fallback must also cover a
`Locked` request that fails for any other reason.

### 6.3 Raw motion is not equally raw

All three relevant backends emit `DeviceEvent::MouseMotion`, but they do not mean the same
thing:

- **X11** — XI2 raw events (`platform_impl/linux/x11/event_processor.rs:1473`). Truly raw.
- **Wayland** — the `zwp_relative_pointer_v1` protocol's *unaccelerated* delta
  (`.../wayland/seat/pointer/relative_pointer.rs:76`). Truly raw.
- **macOS** — `NSEvent.deltaX`/`deltaY` (`platform_impl/macos/app.rs:105-127`). **Already
  through the OS pointer-ballistics curve.**

So identical `sensitivity` gives a different feel on macOS than on Linux, and it is not a
bug in the port. Valve hit exactly this and answered it with `m_mousespeed`,
`m_mouseaccel1` and `m_mouseaccel2` — Windows `SPI_SETMOUSE` overrides, POSIX-inert — and
by defaulting `m_rawinput` to 1 to bypass the problem where it could. Record the
divergence rather than trying to invert macOS's curve.

Corollary, and the reason `CursorMoved` is marked "UI only" above: **view look must come
from `DeviceEvent::MouseMotion`, never from `CursorMoved` deltas.** `CursorMoved` is
clamped to the window and quantised to pixels, so it stalls at screen edges and loses
sub-pixel motion — the classic "can't turn past 180°" bug.

### 6.4 Where in the frame input is sampled — the correctness trap

`FrameClock` refuses frames (`Host::frame` → `None`) whenever `fps_max` says the frame is
early, and `rustdocs/ENGINE.md` gotcha #3 notes this is normal and frequent. `winit`
delivers events regardless of whether a frame runs.

Therefore:

- **Events accumulate into a queue as they arrive** (`Input::push`, from `window_event`).
- **The queue is drained exactly once per engine tick**, at the top of `Engine::frame`,
  *after* `Host::frame` has agreed a frame is happening. That is
  `DispatchAllStoredGameMessages`'s position in `MainLoop` (`sys_mainwind.cpp:509`),
  reproduced.
- **Mouse motion accumulates as a sum, not as a last-value.** Reading per-event and
  applying immediately would make turn speed depend on event rate; keeping only the last
  delta would silently discard motion on every refused frame.

This is precisely what `m_flAccumulatedMouseXMovement` is for, and it is easy to look at
that field, conclude it is legacy sampling cruft, and drop it. **It is not.** Under
`ControlFlow::WaitUntil` pacing it is more necessary than it was for Valve, not less.

One consequence worth stating: the queue must be drained on *shutdown* paths too, or a
`FocusLost` sitting in an undrained queue leaves buttons stuck across a level load.

### 6.5 Focus loss

`WindowEvent::Focused(false)` must release every held button — this is `CInput::ClearStates`
(`in_mouse.cpp:828`). Alt-tabbing with `+forward` held and returning to a player who has
walked into a wall for thirty seconds is the failure. Grab must also be released on focus
loss and re-acquired on focus gain, or the cursor is captured by a window the user has
left.

---

## 7. Key names are external content

`bind "w" "+forward"` lives in shipped `.cfg` files, and `keys.cpp:348`'s
`GetDefaultKeyBindings` parses `scripts/kb_def.lst` from the game content for the default
set. So `s_pButtonCodeName` (`key_translation.cpp:357`) — `"MOUSE1"`, `"MWHEELUP"`,
`"SEMICOLON"`, `"KP_INS"`, `"BACKQUOTE"` — is a **fixed external format**, in
`PORTING.md`'s sense: the mechanism (a `const char*[]` indexed by enum) is ours to
modernize, the strings are not. Transcribe the table verbatim, and test round-tripping
`from_name(name(b)) == b` over every button.

**The one deliberate divergence: bind by physical key.** `winit` splits
`physical_key` (position, layout-independent) from `logical_key` (what the layout says).
Valve's POSIX path collapses them — `PollInputState_Linux` literally does
`ButtonCode_t scanCode = virtualCode` — so on an AZERTY keyboard Valve's `bind w
+forward` binds the key *labelled* W, which is where Q sits on a QWERTY board, and WASD
stops being a square. `winit`'s own `KeyEvent` docs call this out and recommend
`physical_key` for games.

Take `physical_key` for binding, so WASD is always the same four physical keys, and use
`logical_key` only for display and for text entry. **Record it in `rustdocs/`**: a user
who reads `bind` output and sees a name that does not match their keycap needs the
divergence documented, and this is the kind of thing that produces a plausible wrong
behavior rather than an error.

---

## 8. The Rust design

### 8.1 Why `input/` is a fourteenth module

`ENGINE.md` §1 assigns input translation to `window/` and `keys.cpp` to `console/`. Both
assignments were made by grouping Valve's *files*, and both are wrong once the code is
Rust:

1. **`window/` is the `winit` boundary.** `gilrs` is not `winit` — it is a separately
   polled device API with its own event loop contract (§10). A controller backend inside
   `window/` would make `window/` "the platform module" instead of "the windowing
   module", and the next platform input source would land there too.
2. **`console/` owns commands, not button state.** The console's job in this path is
   `Cbuf_AddText` — turn a string into an executed command. The binding table, the
   down-state bitset and the key-up latch are input state that happens to *produce*
   strings. Filing them under `console/` inverts the dependency: `input/` would have to
   live inside the module that consumes it.
3. **`input/` with no `winit` dependency is testable.** The latch, the binding table and
   the name round-trip are pure logic. This is the property that made `host/` testable
   without a GPU, and it is worth the same amount here.

So: `window/` translates, `input/` decides, `console/` executes. Update `ENGINE.md` §1's
module table and §7.4's description when this lands.

### 8.2 Types

Sketch, not a contract — the contract is written in `rustdocs/ENGINE.md` when it lands.

```rust
// A binary input. Flat and densely indexed, per §4.2, without the macro arithmetic.
pub enum Button {
    Key(Key),            // KEY_FIRST..KEY_LAST
    Mouse(MouseButton),  // includes WheelUp/WheelDown as buttons
    // Gamepad { pad: u8, button: GamepadButton },  // stage 5
}

impl Button {
    pub const COUNT: usize;
    pub fn index(self) -> usize;                    // dense, for arrays
    pub fn from_index(index: usize) -> Option<Button>;
    pub fn name(self) -> &'static str;              // "w", "MOUSE1", "MWHEELUP" — §7
    pub fn from_name(name: &str) -> Option<Button>;
}

// InputEvent_t, minus the three-events-per-keypress of §4.1.
pub enum Event {
    Pressed { button: Button, repeat: bool },
    Released(Button),
    Text(char),                       // IE_KeyTyped
    MouseMotion { dx: f32, dy: f32 }, // raw; DeviceEvent only, per §6.3
    CursorMoved { x: f32, y: f32 },   // UI only
    Wheel(f32),
    FocusLost,
}

pub struct Input { /* down bitset, latch, queue, accumulators, capture */ }

impl Input {
    pub fn push(&mut self, event: Event);        // from window/, between ticks
    pub fn frame(&mut self) -> Frame<'_>;        // drained once per tick, §6.4
    pub fn is_down(&self, button: Button) -> bool;
    pub fn take_mouse_delta(&mut self) -> (f32, f32);  // sum since last call
    pub fn clear(&mut self);                     // ClearStates
}

// Stage 2. Temporary home — see §11.
pub struct ViewAngles { pub pitch: f32, pub yaw: f32, pub roll: f32 }
impl ViewAngles {
    pub fn apply_mouse(&mut self, dx: f32, dy: f32, sensitivity: f32); // clamps pitch
}
```

`push`/`frame` is `PostEvent`/`DispatchAllStoredGameMessages`, and the split is what makes
§6.4 structurally correct rather than remembered.

### 8.3 The three seams

**To `console/` — `CommandSink`.** Stage 3 needs `Cbuf_AddText`, which does not exist.
Rather than wait, define the seam:

```rust
pub trait CommandSink {
    fn enqueue(&mut self, command: &str);
}
```

`input/` formats the string and hands it over; `console/` implements the trait later. This
is deliberately the same move `host::Level` makes — the module that is ready does not wait
for the module that is not, and the seam is a trait with two methods rather than a
scaffold to delete. Before `console/` exists, a sink matching a hard-coded command set is
enough to prove bindings work.

Keep the `+`/`-` convention exactly (`keys.cpp:1147-1170`): a binding starting with `+`
sends `+forward <index>` on press and `-forward <index>` on release, **with the button
index as an argument**. That argument is not decoration — it is what `kbutton_t::down[2]`
matches on (§4.4) so that releasing one of two keys bound to the same command does not
stop the movement.

**To egui — one boolean, not a chain.** `DispatchInputEvent`'s VGui → RocketUI → GameUI
precedence collapses to egui's "did the UI consume this event". But **keep the latch**
(§4.3): the answer must be recorded per button on the down event and replayed on the up,
or the console-open-while-firing bug returns. `Consumed::{Ui, Game}` in a
`[Option<Consumed>; Button::COUNT]` is the whole of it.

**To `client/` — nothing yet, on purpose.** `Input::is_down` plus the fractional
`KeyState` model (§4.4) is what `CUserCmd` will be built from. Do not anticipate it.

---

## 9. Staged plan

Each stage is independently reviewable and independently useful.

1. **Translation and state.** `Button`, `Key`, `Event`, `Input`; the name table and its
   round-trip test; `window/` translating `WindowEvent` and implementing
   `ApplicationHandler::device_event`; the queue drained at the top of `Engine::frame`.
   No bindings, no camera change, no capture.
   *Deliverable:* `-input_debug` (or equivalent) printing button names and mouse deltas.
   *Tests:* name round-trip over all buttons; press/release state machine including the
   redundant-transition guard (§4.3); deltas accumulating across refused frames.
   *Not here:* capture, bindings, any camera behavior.

2. **Look and move — the placeholder camera dies.** Mouse capture with the
   `Locked`→`Confined`+warp fallback (§6.2); `ViewAngles` with `m_pitch`/`m_yaw`/
   `sensitivity` and the `cl_pitchdown`/`cl_pitchup` clamp; a free-fly camera in
   `Engine::camera` driven by held buttons; focus loss releasing everything.
   *Deliverable:* WASD + mouse fly-through of `sp_a1_intro1`. **`TURN_RATE` and the
   turntable comment in `src/engine/mod.rs` are deleted by this stage** — they say
   in-source that they will be.
   *Also here:* Escape releases the mouse (with no UI yet, there is otherwise no way to
   get the cursor back — do not skip this).
   *Then:* write `rustdocs/ENGINE.md`'s `input` section. The module is real at this point
   and the API doc is not optional (`CLAUDE.md`).

3. **Bindings and command dispatch.** — **done.** The binding table, `+`/`-` with the
   index argument, `CommandSink`, `bind`/`bind_osx`/`unbind`/`unbindall`,
   `key_listboundkeys`/`key_findbinding`. `console/` landed between stages 2 and 3 as
   expected.
   **`kb_def.lst` turned out not to be the default binding set** and was not ported:
   `GetDefaultKeyBindings` (`keys.cpp:348`) parses it into a *command → suggested key*
   map that `GetSuggestedBinding` feeds to the options UI for commands that are unbound.
   The actual defaults are `cfg/config_default.cfg`, which `exec` reads. It comes back
   with an options UI, not before.

4. **UI precedence.** egui's consumed-answer plus the key-up latch (§4.3, §8.3). *Wants
   the egui integration*, which is not scheduled.

5. **Controllers.** §10.

Stages 1 and 2 are the first landing and are what "port input over" means in this
request. 3 onwards are gated on modules that do not exist.

**Stages 1-3 landed as planned.** Stage 3 (bindings) arrived with `console/` stage 2, as
this plan expected — `console/` supplies `CommandSink`, the table lives in `input/`, and
WASD now comes from `cfg/config_default.cfg`. Two things about stage 3 are worth recording
because the plan did not anticipate them:

- **`kbutton_t`'s two-holder set had to be ported after all**, in
  `input::view::KButton`. §4.4 defers `kbutton_t` wholesale to `client/`, but the index
  argument the `+`/`-` convention carries is *only* meaningful against `down[2]` — without
  it, two keys bound to `+forward` cancel each other and the argument is decoration. The
  fractional `KeyState`, which is the genuinely client-shaped half, is still deferred.
- **Portal 2 binds no vertical movement.** `config_default.cfg` has `+jump` and `+duck`
  but neither `+moveup` nor `+movedown`, and `ComputeUpwardMove` (`in_main.cpp:1101`)
  reads only the latter pair. `MoveButtons` accepts jump and duck as the placeholder
  camera's up and down; it is documented as a divergence and dies with `client/`.

Stages 1 and 2 landed with three additions worth recording here because
they change what a later stage inherits:

- **`Event` gained `FocusGained`.** Valve had no need for it; this port does, because
  X11 delivers raw motion from the *device* whether or not the window is focused, so
  `Input` must gate accumulation on focus rather than trusting the source.
- **Motion is dropped at `push`, not filtered later**, for the same reason, and because
  that is also `ResetMouse`'s swallow-the-jump behavior on re-capture.
- **`window/` owns a `capture_wanted` edge**, so a grab the platform refuses is reported
  once rather than retried every frame (§6.2 describes the fallback but not the retry).

Not landed, and deliberately: no `CommandSink` (§8.3) — nothing would implement it yet,
and an unused trait is scaffolding.

---

## 10. Controllers — `gilrs`, stage 5, deliberately deferred

**Not part of the first landing.** Recorded now so stages 1–2 leave the right room and so
nobody re-derives the split.

**What `gilrs` replaces:** the device layer only — `joystick_linux.cpp` (566),
`joystick_osx.cpp` (1,025), `joystick.cpp` (368) and `xcontroller.cpp` (2,236) ≈ **4,195
lines of per-platform enumeration, axis mapping and per-pad quirks**. `gilrs` covers
Linux (evdev) and macOS (IOKit), and normalizes pads through the SDL game-controller
mapping database, so a pad reports named buttons and axes rather than the raw indices
`xcontroller.cpp` hand-mapped per device.

**What `gilrs` does *not* replace:** `in_joystick.cpp` (2,016 lines). That is response
curves (`joy_response_move`, six modes), deadzones (`joy_*threshold`, 0.15),
sensitivity (`joy_yawsensitivity`, `joy_pitchsensitivity`), acceleration promotion, and
auto-aim dampening. It is **content-tuned client-layer behavior** — Portal 2 ships its own
defaults for these — and it belongs with `CUserCmd` in `client/`. Porting it into
`input/` at stage 5 would put gameplay feel in the device module.

**The architectural consequence that stages 1–2 must respect:** `gilrs` is *polled*, not
pushed. There is no `winit` callback; you call `Gilrs::next_event()` in a loop until it
returns `None`. So the tick must have a place to drain a second event source into the same
queue before `Input::frame` runs. Stage 1's `push`/`frame` split already provides it — but
only if `push` stays public and the queue is not private to `window/`. **That is the one
thing stage 1 must not get wrong**, and it costs nothing to get right.

Other notes for whoever does this:

- `Button` gains `Gamepad { pad: u8, button: GamepadButton }` and the analog axes become
  real axes, not `JOYSTICK_AXIS_BUTTON` fake buttons (§4.2). Binding a *stick* to
  `+forward` is not a thing that needs supporting; binding a face button is.
- Hot-plug is `gilrs`'s `Connected`/`Disconnected` events, replacing
  `IE_ControllerInserted`/`IE_ControllerUnplugged`.
- Rumble is `gilrs`'s `ff` feature, if it is ever wanted; `in_forcefeedback.cpp` is X360
  and stays deleted.
- **`Cargo.toml` requires a justification comment** for every dependency, in the style of
  the existing six. Write it: what it replaces, why not hand-rolled, and what its own
  dependency footprint is (`gilrs` is not zero-dependency, unlike `glam` and `bytemuck` —
  say so).
- Steam Controller (`steamcontroller.cpp`, `in_steamcontroller.cpp`) is a *separate*
  question that arrives with Steam integration, not with `gilrs`.

---

## 11. Open questions and risks

1. **Split-screen.** Neither `PORTING.md` nor `ENGINE.md` decides it, and Portal 2 has
   split-screen co-op. The input-side cost of the answer is asymmetric: the *binding
   table is global* in Valve's design (`Key_SetBinding` has no slot parameter), and the
   player slot is derived per event — from `GetJoystickForCode` for controllers and from
   `in_forceuser` for keyboard/mouse (`keys.cpp:1258-1272`). So the decision reduces to
   whether the *down-state and view angles* are one object or an array, which is cheap to
   defer **provided nothing bakes "one player" into `Event` or `Button`.** Recommendation:
   build for one player, keep the seam, do not add a slot field until co-op is scheduled.
2. **`ViewAngles` in `input/` is a wart with a known end.** They belong in
   `CClientState` (§4.6). **Move them to `client/` when it exists**; until then there is
   nowhere else, and the alternative — putting them in `engine/mod.rs` — spreads the same
   code over two modules instead of one. Comment it at the site, in the style of
   `CLAUDE.md`'s "Known warts" section, and add it to that section when stage 2 lands.
3. **`fps_max` versus input latency.** With `fps_max` 300 and a 1000 Hz mouse, several
   motion events accumulate per frame — correct, and the accumulator handles it. But
   `Host::frame` refusing frames means input is sampled at the *frame* rate, not the
   event rate, so lowering `fps_max` raises input latency. That is faithful to Valve and
   worth stating in `rustdocs/` before someone reports it.
4. **`MouseScrollDelta::LineDelta` vs `PixelDelta`.** macOS trackpads report pixels and
   report them continuously; a mouse wheel reports lines and reports them discretely.
   `MWHEELUP`/`MWHEELDOWN` are *discrete* buttons. Pixel deltas need a threshold-and-latch
   to avoid firing hundreds of `MWHEELUP` presses per trackpad swipe. Not hard; easy to
   miss until a Mac trackpad is tried.
5. **Text entry versus bindings, on the same key.** `winit` gives `text` alongside the key
   event, so both are available on one event — but the console needs "w types a w" while
   the game needs "w is `+forward`", and the switch between them is the egui-consumed
   answer of stage 4. Until then, stage 3 must not route `Text` anywhere.
6. **Key repeat.** `event.repeat` is passed through in §8.2 rather than filtered, because
   the console wants repeat and bindings must not have it (`kbutton_t`'s KeyDown returns
   early on a repeat, `in_main.cpp:434`). Whoever consumes the event decides; do not
   filter at the source.

---

## 12. Notes for whoever picks this up

- **Graph coverage on these files is good but not complete**, per
  `check_index_coverage`. Every flagged range is a single line, and in `engine/keys.cpp`
  the flagged lines are *precisely* the `CON_COMMAND` macro registrations (165, 191, 211,
  234, 259, 328, 333, 816, 838). **The graph therefore under-reports this module's console
  commands specifically** — do not conclude from a `search_graph` miss that a command does
  not exist. `inputsystem.cpp`, `key_translation.cpp`, `sys_mainwind.cpp`, `in_main.cpp`,
  `in_mouse.cpp` and `in_joystick.cpp` are also `parse_partial` at isolated lines;
  `public/inputsystem/ButtonCode.h` and `iinputsystem.h` are clean. Everything quoted in
  this document was read from source directly.
- Line numbers are from the tree at time of writing; re-verify before relying on them.
- `winit` facts in §6.2 and §6.3 were verified against
  `~/.cargo/registry/src/*/winit-0.30.13/` — `src/window.rs:1687` for the grab modes and
  `src/platform_impl/{macos/app.rs,linux/x11/event_processor.rs,linux/wayland/seat/pointer/relative_pointer.rs}`
  for the motion sources. **Re-check both on any `winit` upgrade**; `Cargo.toml` already
  notes that 0.31 changes `ApplicationHandler`, and grab-mode support is exactly the kind
  of thing that changes between releases.
- Only POSIX paths are documented, per `PORTING.md`. `inputsystem/` is unusually dense
  with `_X360`/`_PS3`/`WIN32` branching even by this tree's standards — roughly half the
  directory is hardware that is out of scope — so skim for `LINUX`/`OSX`/`POSIX` and
  unconditional code and discard the rest aggressively.
