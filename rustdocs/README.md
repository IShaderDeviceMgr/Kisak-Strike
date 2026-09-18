# rustdocs/

API references for the Rust subsystems that have actually been implemented under `src/`.

**These describe what exists. [`portdocs/`](../portdocs/) describes what to build.** The
two are deliberately separate:

| | `portdocs/<MODULE>.md` | `rustdocs/<MODULE>.md` |
|---|---|---|
| Written | *before* the port | *with* the port, updated as it changes |
| Subject | the C++ in `legacy/` | the Rust in `src/` |
| Contains | inventory, sizes, what to port/replace/delete, staged plan, open questions | public types and signatures, usage, invariants, gotchas, what's deferred |
| Answers | "how do I port this?" | "how do I *use* this?" |
| Lifetime | can go stale once the module lands | must stay accurate forever |

A module gets a `rustdocs/` entry once it has a public API other subsystems will call.
Naming matches `portdocs/`: `src/filesystem/` → `rustdocs/FILESYSTEM.md`.

**Each of the five larger docs ends with "What has landed, and what each stage
found"** — the narrative of what was ported in what order, what it cost and what
the measurements said. That history used to live in `CLAUDE.md`, which had grown
to 2,126 lines and become too large to be useful as an orientation file; it now
keeps a status table and points here. The invariants-and-gotchas lists are the
normative part of each document, and those stayed where they were.
`ENGINE.md` also holds two things that are nobody's module in particular:
**"Frame cost, measured"** and **"What the binary does, and what `sp_a1_intro1`
draws"** — the standing census of the default map.

## Current docs

- [`FILESYSTEM.md`](FILESYSTEM.md) — `src/filesystem/`. `Vfs`, `PathId`, mounts,
  `gameinfo.txt` parsing, the KeyValues reader, and VPK reading.
- [`MATERIALS.md`](MATERIALS.md) — `src/materials/`. `Renderer` and the frame boundary,
  the `wgpu` decisions the rest of the renderer inherits, the texture path (`Vtf`,
  `ImageFormat`, `Texture`, `TextureCache`), the material path (`Vmt`, `MaterialVar`,
  `ShaderKind`, `PipelineCache`, `Material`), meshes and the render context, the
  lightmap atlas, and the post-processing chain — the offscreen scene target, the GPU
  luminance histogram the tone mapper measures, and the pass that presents it. Its porting doc is
  [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named after the C++
  module, while this one is named after the Rust module.
- [`ENGINE.md`](ENGINE.md) — `src/engine/`. `window` (the `winit` event loop,
  `VideoConfig`, frame pacing), `host` (the state machine and frame clock), `world` (the
  `.bsp` reader, lightmap packing, the batches a map draws as, and `disp`'s terrain),
  `trace` (ray and
  swept-box traces against the world's brushes and against brush models, and the
  `DIST_EPSILON` behaviour movement depends on), `input` (buttons, bindings, the key-up latch) and `console` (cvars, the
  command buffer, the dialog), plus `Engine` itself and how the frame is composed.
- [`CLIENT.md`](CLIENT.md) — `src/client/`. The **game client**, and the first game module
  in the tree: `Client`, `UserCmd`, `Buttons`/`KButton`'s fractional `KeyState`, `Player`,
  `ViewAngles`, and the movement — `FullNoClipMove` and `FullWalkMove` with gravity,
  friction, stair stepping, jumping and ducking. Records the ten places
  **`CPortalGameMovement` differs from `CGameMovement`** in ways that have nothing to do
  with portals — plus `ToneMap`, the auto-exposure controller
  (`portdocs/CLIENT_TONEMAP.md`), which is policy over a histogram and names no GPU type.
  Not to be confused with `ENGINE.md` §7.5's *client connection*, which does not exist yet.
- [`SERVER.md`](SERVER.md) — `src/server/`. The **game server**, and the second game
  module: `Server` and the level lifecycle, `EntityList`'s generational handles, `Entity`
  split into `EntityCore` plus a `Behaviour`, `ClassDef` and the ten classes implemented
  so far, the `CBaseEntity::KeyValue` ladder with Valve's `atoi`/`atof` prefix semantics,
  and name matching. Stage 1 of five: entities are created, parsed, spawned and
  activated; entity I/O, thinks, movement and touch are stages 2-4. Records that
  **inheritance became composition** (there is no datadesc chain to walk), that
  "unhandled key" is not "unimplemented" (17 of the 28 in the whole game are the map
  compiler's), and that `names_match`'s `*` does not have to be trailing whatever the
  comment in `baseentity.cpp` says. Names no GPU type, so all 28 of its unit tests run
  without a window.

## Root modules

Two modules live at the crate root rather than under a subsystem, because their consumers
span siblings that cannot see each other. Neither is large enough to earn its own file,
so they are recorded here:

- **`src/cmdline.rs`** — `CommandLine()`. Valve kept it in `tier0` because *everything*
  reads it; it moved out of `launcher/` when `engine::console` became its third consumer
  (`stuffcmds` and the `+<cvar>` default seeding). `value()` refuses a value beginning
  with `-` or `+`, matching `CCommandLine::ParmValue` (`tier1/commandline.cpp:646`) —
  without which `-window` swallows `+map`.
- **`src/math.rs`** — `angle_matrix(angles: Vec3) -> Mat3`, its inverse
  `matrix_angles(matrix: Mat3) -> Vec3` (`MatrixAngles`), and
  `vector_angles(forward, pseudo_up) -> Vec3` (`VectorAngles`).
  `PORTING.md` replaces `mathlib` with `glam`, and almost all of it goes; what does not is
  the handful of places Valve fixed a *convention* rather than doing arithmetic. **A
  `QAngle` is pitch, yaw, roll in degrees** and composes as `Rz(yaw) · Ry(pitch) ·
  Rx(roll)` (`mathlib_base.cpp:1329`'s own comment: `matrix = (YAW * PITCH) * ROLL`).
  Built from three explicit axis rotations, never `Mat3::from_euler`, because every
  `EulerRot` variant encodes an intrinsic/extrinsic convention as well as an order and the
  wrong one is a silent half-right answer — anything with only a yaw looks correct under
  any reading, and only tilted things are wrong. The result multiplies on the left
  (`VectorRotate`); its transpose is the inverse (`VectorIRotate`). Consumers:
  `engine::world::props` (a prop's `model_to_world`) and `engine::trace` (a brush model's
  frame). `AngleVectors` — `client::view::ViewAngles::vectors` — is a *different* function
  and has stayed where its one caller is; move it here if it gets a second.

  **`matrix_angles` and `vector_angles` are not interchangeable**, and they disagree on
  purpose: `MatrixAngles` reads the basis out of a matrix and `VectorAngles` builds one
  from a direction and a roll reference, and in the gimbal-locked case (forward is nearly
  `±Z`) the second negates the yaw, carrying Valve's own note that the copy taken from the
  first was found to be 180° out. The general case agrees, which
  `matrix_angles_reads_back_what_angle_matrix_wrote` pins. `matrix_angles`' caller is the
  portal teleport: an angle set goes through the pair's matrix by being turned into a
  rotation, composed and read back out (`client::movement::Teleport::turn`).

## Why these exist

Porting sessions lose context. A cold-started session that has to re-derive an API by
reading 3,500 lines of source will burn most of its budget doing so, and is likely to
miss the non-obvious rules — which lookups skip which mounts, why a path type carries two
spellings of the same string, which behaviors deliberately diverge from Valve's. Those
belong in prose, once, next to the code they describe.

Rustdoc comments in the source stay the authority on individual items; these files carry
the parts that don't fit on a single item — cross-cutting semantics, worked examples, and
the "why is it like this" that a `///` on one function can't hold.
