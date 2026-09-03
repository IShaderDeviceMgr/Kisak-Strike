# `src/engine/` — API reference

The engine. [`portdocs/ENGINE.md`](../portdocs/ENGINE.md) breaks the original `engine/`
module into 23 subsystems, 13 of which become modules here. **One of them exists so far.**

| Module | Subsystem | Status |
|---|---|---|
| [`window`](#engine-window) | `sys_mainwind.cpp`, `sys_getmodes.cpp`, `sdlmgr.cpp`, `inputsystem/` | window + event loop done; input not started |
| `host/`, `net/`, `world/`, `console/`, `audio/`, … | the other 12 | not started |

---

<a id="engine-window"></a>

## `src/engine/window/`

The game window and the event loop that drives the frame. Replaces `CGame`
(`engine/sys_mainwind.cpp`), `CVideoMode` (`engine/sys_getmodes.cpp`), `CSDLMgr`
(`appframework/sdlmgr.cpp`), `appframework/cocoamgr.mm`, `inputsystem/` and the vendored
`thirdparty/SDL2`.

| | |
|---|---|
| Module | `crate::engine::window` |
| Lines | ~640 including tests |
| Tests | 12 (`cargo test engine::window`) |
| Dependencies | `winit` 0.30, `crate::materials`, `crate::filesystem`, `crate::launcher::cmdline` |

### Quick start

```rust
use crate::engine::window::{run, VideoConfig};

let video = VideoConfig::from_command_line(&cmdline, game_title.as_deref());
run(video, vfs.as_ref(), cmdline.value("-vtf"))?;   // returns when the window closes
```

`src/launcher/mod.rs` is the only caller, and this is where `CEngineAPI::Run`/`MainLoop`
(`engine/sys_dll2.cpp:1132`) took over in the original.

### `run`

```rust
pub fn run(
    config: VideoConfig,
    vfs: Option<&Vfs>,
    test_texture: Option<&str>,
) -> Result<(), WindowError>;
```

Creates the event loop, the window and the renderer, and runs until the window closes.
**Must be called from the main thread** — a hard AppKit requirement on macOS that `winit`
enforces on every platform.

`vfs` is the mounted game content, and is `Option` because a failed mount is survivable:
the launcher reports it and boots the window anyway, since a window that opens and says
what is wrong beats a process that exits. `test_texture` is `-vtf <name>`, **stage 2 of
`portdocs/MATERIALSYSTEM.md` §9's verification path**: it loads `materials/<name>.vtf`
and draws it over the frame, falling back to the error checkerboard if anything about
that fails (including there being no `vfs` at all). Both that parameter and
`crate::materials::TextureBlit` are deleted when stage 3's material path can draw a quad
through a real `.vmt`.

The signature will keep growing until there is an engine to hand these to — that is the
seam `CEngineAPI::Run` occupied, and it is not yet a real one.

A failure inside a `winit` callback cannot be returned from that callback, so a startup
error is parked on the handler, the loop is asked to exit, and the error is re-raised
from `run`.

### `VideoConfig`

```rust
pub struct VideoConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
    pub borderless: bool,          // -noborder; windowed mode only
    pub resizable: bool,
    pub adapter_index: Option<usize>,
    pub vsync: bool,
}

pub fn from_command_line(cmdline: &CommandLine, game_title: Option<&str>) -> Self;
```

The video half of `MaterialSystem_Config_t`, minus everything describing 2004-era
hardware variety. `from_command_line` is a port of
`OverrideMaterialSystemConfigFromCommandLine` (`engine/matsys_interface.cpp:356`) plus
the title handling from `CGame::CreateGameWindow` (`engine/sys_mainwind.cpp:1253`);
`game_title` is `gameinfo.txt`'s `game` key, which the launcher reads.

Switches honoured, all spelled as the original spelled them:

| Switch | Effect |
|---|---|
| `-sw`, `-startwindowed`, `-windowed`, `-window` | windowed |
| `-full`, `-fullscreen` | fullscreen (only if no windowed switch was given) |
| `-width`/`-w`, `-height`/`-h` | size |
| `-noborder` | undecorated window |
| `-resizing` | resizable window |
| `-mat_vsync <0\|1>` | vsync |
| `-adapter <n>` | GPU selection |
| `-window_name_suffix <s>` | appends `" - <s>"` to the title |
| `-safe` | windowed 640x480 |

### `WindowError`

`EventLoop(winit::error::EventLoopError)`, `Creation(winit::error::OsError)`,
`Renderer(crate::materials::RendererError)` — the last is `#[error(transparent)]`, so a
GPU startup failure reads as itself.

## Divergences from Valve's behavior

Each is deliberate; each names the switch that reverses it.

| Setting | Valve's constructor | Here | Override |
|---|---|---|---|
| Windowed | fullscreen | **windowed** | `-fullscreen` / `-full` |
| Size | 640x480 | **1280x720** | `-width`/`-w`, `-height`/`-h` |
| Vsync | off (`NO_WAIT_FOR_VSYNC` set) | **on** | `-mat_vsync 0` |
| Resizable | off | off (unchanged) | `-resizing` |

The reason they can differ at all: Valve's real defaults come from `videoconfig.cfg` via
`ReadVideoConfig()`, and the constructor's values
(`public/materialsystem/materialsystem_config.h:140`) are placeholders that never survive
to window creation. We have the constructor and not the config file, so reproducing 640x480
would be cargo-culting rather than fidelity. **When the video config file is read, revisit
all four.**

Vsync is the one worth arguing about: with no `fps_max` and no `FilterTime` yet, vsync is
the *only* thing pacing the frame loop.

Also dropped: the `" - OpenGL"` title suffix (`sys_mainwind.cpp:1271`), which advertised
the `togl` path that no longer exists. The renderer logs the live backend instead.

## Behaviors carried across that look like bugs

Both are in `from_command_line`, both are guarded by tests:

- **`-width` without `-height` forces 4:3** (`height = width * 3 / 4`,
  `matsys_interface.cpp:386`) rather than keeping the previous height. `-w 800` really
  does give you 800x600.
- **`-w` beats `-width`, `-h` beats `-height`** when both spellings are present, because
  the original reads them in that order into the same field
  (`matsys_interface.cpp:382-392`).

## Invariants and gotchas

1. **`run` must be on the main thread.** See above.
2. **A skipped frame must back off, and `WindowEvent::Occluded` is not the signal to use.**
   When `Renderer::begin_frame` returns `None`, `draw` arms a `SKIP_RETRY` (100 ms)
   deadline, `about_to_wait` turns that into `ControlFlow::WaitUntil` and — critically —
   does *not* request a redraw before it expires, since a pending redraw request wakes
   the loop and defeats the deadline.

   This was gotten wrong first, so it is worth stating why: the original version keyed
   off `WindowEvent::Occluded` instead. On macOS a window covered by another application
   fails every frame acquisition **without** producing that event, so the handler waited
   for an event that never came and spun at 100% of a core, measured at ~75,000 failed
   acquisitions per second, rendering nothing. The surface is the only always-correct
   signal, and the retry is also how un-occlusion gets noticed — which is why it backs
   off rather than idling until an event arrives.
3. **Pacing lives here, not in the engine.** `FilterTime`'s *policy* (`fps_max` and
   friends, `engine/sys_engine.cpp:264-411`) has not been ported yet — vsync is currently
   the only frame limiter — but its mechanism, `ControlFlow::WaitUntil`, is already in use
   for #2 and is where `fps_max` will apply as a second deadline. **Nothing in
   `src/engine/window/` may sleep**; that is exactly the two-systems-both-pacing failure
   `portdocs/ENGINE.md` §6 warns about.
4. **Quit vs. restart is not modelled.** `event_loop.exit()` currently always means
   "exit", so `run` returning `Ok` means "the window was closed". `SetQuitting(QUIT_RESTART)`
   has to become a distinct outcome — in `run`'s return type — before the original's
   restart loop can exist in `src/launcher/`.
5. **Field order in the internal handler is load-bearing.** The renderer is declared
   before the window so the surface is dropped before the window it points at.
6. **`CommandLine` is imported from `crate::launcher`.** It sits there because that is
   where it is built, but Valve kept `CommandLine()` in tier0 precisely because
   everything reads it. If a third subsystem needs it, move it to a crate-level
   `src/cmdline.rs` rather than growing more of these imports.

## Not implemented

- **Input.** `WindowEvent`'s keyboard/mouse variants are dropped on the floor. The chain
  they replace (`Key_Event` → VGui → RocketUI → GameUI, `sys_mainwind.cpp:399`) and the
  UI-precedence design question `egui` raises are in `portdocs/ENGINE.md` §6.
- **The engine tick.** `about_to_wait` currently requests a redraw and `RedrawRequested`
  clears one frame. That draw call is the seam where one engine tick will go.
- **Multiple windows / `AddView`.** Never coming back; the original needed them for
  Hammer.
- **Window icon** (`SetApplicationIcon` from `resource/game-icon.bmp`,
  `sys_mainwind.cpp:1297`), **monitor and video-mode enumeration**, `-refresh`.

## Test coverage

12 tests, all on `VideoConfig::from_command_line` — the only part that is pure logic.
Anything touching `winit` or `wgpu` needs a display and a GPU, so it is verified by
running the binary (see [`MATERIALS.md`](MATERIALS.md#test-coverage)).

| Test | Guards |
|---|---|
| `defaults_are_a_windowed_720p_window` | every divergence in the table above |
| `every_windowed_switch_spelling_is_accepted` | all six mode switches, and windowed winning over `-fullscreen` |
| `width_without_height_forces_four_by_three` | the 4:3 quirk |
| `short_forms_win_over_long_ones` | `-w`/`-h` precedence |
| `short_height_alone_suppresses_the_four_by_three_rule` | that `-h` counts as "height was given" |
| `unparsable_sizes_fall_back_rather_than_producing_a_zero_size_window` | gotcha #1 in `MATERIALS.md` — a 0 would panic `Surface::configure` |
| `safe_mode_overrides_everything_before_it` | `-safe` ordering |
| `the_title_comes_from_gameinfo`, `window_name_suffix_is_appended` | title composition and its fallbacks |
