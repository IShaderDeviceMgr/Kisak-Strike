# Porting console, cvars & commands → `src/engine/console/`

**Status: stage 1 landed** (§8.1 — cvars, the command buffer, both tokenizers,
dispatch, aliases, `exec`, `stuffcmds`, the log sink, and `map`/`quit`/`restart`
through a `CommandTarget`). Stages 2-5 are not started. This is the plan,
written before the port, per `PORTING.md`'s per-module rule; **the API reference
in `rustdocs/ENGINE.md`'s `src/engine/console/` section is the document to read
in order to *use* the module**, and it is authoritative where the two disagree.

Four things in this plan turned out to be wrong at the code, and are corrected
there rather than rewritten here — see its "Corrections to
`portdocs/ENGINE_CONSOLE.md`" list. In short: **§4.5's file-extension check is a
blocklist, not a `.cfg`/`.rc` allowlist**; **§6.4's `Command<'a>` cannot borrow
from the buffer** (Valve's own `memcpy` at `convar.cpp:421` is the same problem);
**§4.2's queue cap does not catch an alias that expands to itself**, which needs
a separate per-round budget; and `CCommandLine::ParmValue` refuses a value
starting with `-`/`+`, which `src/cmdline.rs` did not and `stuffcmds` depends on.

Written against the current architecture (single crate, no FFI, `winit`/`wgpu`);
nothing here assumes the old FFI-bridged model.

This is `portdocs/ENGINE.md` §7.4, and — like `ENGINE_INPUT.md` — it is the doc
for one system that the original spreads across three modules that only make
sense together:

| Layer | Original | Job |
|---|---|---|
| Objects | `tier1/convar.cpp` (1,531) + `public/tier1/convar.h` (1,169) | what a `ConVar`/`ConCommand` *is*; the `CCommand` tokenizer |
| Registry | `vstdlib/cvar.cpp` (1,317) | name → object, registration, iteration, change callbacks |
| Buffer | `tier1/commandbuffer.cpp` (407) | queued command text, ticks, `wait` |
| Policy | `engine/cmd.cpp` (1,171) + `engine/cvar.cpp` (1,425) | dispatch order, aliases, `exec`, who is allowed to run what |
| Output | `engine/console.cpp` (1,652) | `Con_Printf` and everything hanging off it |
| UI | `vgui2/vgui_controls/consoledialog.cpp` (1,371) | the dialog, history, autocomplete |

Naming follows `ENGINE.md` §10's `ENGINE_<SUB>.md` convention for engine
submodules, alongside `ENGINE_INPUT.md`.

Unqualified `§n` references are to sections of *this* document; references to
other docs are named (`ENGINE.md` §7.4).

**Read `PORTING.md` first**, then `rustdocs/ENGINE.md` (the frame and the host
state machine this hooks into) and `portdocs/ENGINE_INPUT.md` §8.3 (the
`CommandSink` seam this is the other half of).

---

## 0. Headline decisions

1. **`console/` owns no engine state and names no engine type.** Like `host/`,
   it depends on `std` alone and is tested without a window, a GPU or a mounted
   filesystem. That is possible because of decisions 3 and 4.
2. **There is no global cvar registry, and no `OnceLock`.** `ENGINE.md` §7.4
   said the cvar registry "is the one piece of ambient global state that is
   genuinely process-global and justifies a `OnceLock`." **That call is
   reversed here**, and §6.1 is the argument: a cvar is a shared *cell*
   (`Arc`-held, atomics inside), so a subsystem that wants `mat_luxels` holds
   its own handle to that one cvar rather than reaching a registry for it. The
   registry then only serves *name* lookup — the console, and nothing else,
   needs it.
3. **A cvar set is handled inside `console/`; a command is handed back out.**
   Setting `fps_max 60` requires nothing but the cvar. Running `map
   sp_a1_intro1` requires `&mut Host`. So dispatch resolves aliases and cvars
   itself and returns commands to a `CommandTarget` — the same trait-seam move
   `host::Level` makes, for the same reason.
4. **A command is not a callback.** Valve's `FnCommandCallback_t` is a bare
   function pointer that reaches its state through globals; there are no
   globals here, and a closure that captures `&mut Engine` cannot be stored in
   a registry that `Engine` owns. Commands are *declared* as data
   (`CommandSpec`: name, help, flags, completion) and *executed* by whoever
   owns the state. §6.3.
5. **Splitscreen is out of scope, so there is one command buffer.** Valve has
   `CBUF_COUNT` of them (`cmd.h:149`) — one per splitscreen player plus one for
   the server — plus `FCVAR_SS`, `CSplitScreenAddedConVar`, `SplitScreenConVarRef`
   and the `cmd1`…`cmd4` prefix commands. All of it goes (§5).
6. **The dev-console *UI* is a later stage and wants `egui`.** Stage 1 is the
   machinery: everything `valve.rc` needs in order to run. You can type nothing
   until stage 4, and that is fine — `exec`, `stuffcmds` and bindings are what
   actually unblock the boot path.
7. **`console/` owns the print sink.** `Con_Printf` needs somewhere to go, and
   the scrollback the UI eventually renders should not have to be retrofitted
   into every call site. A bounded ring of coloured lines plus the stderr echo
   the port already does. **`tracing` is not adopted yet** — §9, open question 3.
8. **The 32 `FCVAR_*` flags are not ported as a block.** Ten survive now, ten
   arrive with `net/`, twelve are deleted. §4.6 is the flag-by-flag table, and
   it is the single most useful thing in this document to get right, because a
   dropped flag is a security hole (§4.7) and an invented one is dead weight.

---

## 1. Scope: what "console" means here

Four things travel together and one does not:

- **Cvars** — named, typed, flagged, persisted settings. 4,170 `ConVar`
  declarations across the tree.
- **Commands** — named actions with arguments. 849 `CON_COMMAND*` uses.
- **The command buffer** — the queue that turns typed or scripted *text* into
  those, at a defined point in the frame.
- **Console output** — `Con_Printf` and the ring it writes into.

Not in scope, and deliberately: **the console dialog**, which is `egui`'s
(`PORTING.md`: `egui` replaces vgui2, RocketUI and ScaleformUI at once), and the
**notify area** (`CConPanel`, the fading lines at the top of the screen), which
is a HUD element and belongs wherever the HUD lands.

---

## 2. Inventory

Line counts are `wc -l` against the tree at time of writing.

### `tier1/` + `public/tier1/` — 3,150

| File | Lines | Disposition |
|---|---|---|
| `public/tier1/convar.h` | 1,169 | **Knowledge survives, encoding does not.** `ConCommandBase`/`ConCommand`/`ConVar`'s triple inheritance, the intrusive `m_pNext` list, `CCommand`'s fixed buffers, `ConVarRef`, the splitscreen variants. §4.8. |
| `tier1/convar.cpp` | 1,531 | The value semantics survive (`InternalSetValue`, `ClampValue`, the float/int/string triple cache); the registration half is deleted with the DLL model. |
| `tier1/commandbuffer.cpp` | 407 | **Port the behavior faithfully** — tick scheduling, `wait`, insert-during-processing. §4.2. |
| `public/tier1/commandbuffer.h` | 127 | Dissolves into the Rust type. |
| `public/tier1/iconvar.h` | 126 | The **flag set** (§4.6) is the content; `IConVar` itself is deleted. |
| `public/tier1/convar_serverbounded.h` | 53 | **Deferred to `net/`** — a client cvar the server clamps. |
| `tier1/characterset.cpp` + `.h` | 84 | Deleted — a 256-byte lookup table for `{}()':`. `str::split`/`match` is this. |

`tier1/utlbuffer.cpp` (1,914) is not a port target, but **`CUtlBuffer::ParseToken`
(`:1357`) is the actual tokenizer** and has to be read to port `CCommand` — §4.3.

### `vstdlib/` + `public/` — 1,770

| File | Lines | Disposition |
|---|---|---|
| `vstdlib/cvar.cpp` | 1,317 | `CCvar`, the registry. **Roughly half is the cross-DLL parent/child linkage** (§4.8) and the material-thread queue, both of which evaporate. What survives: registration, lookup, iteration, `RevertFlaggedConVars`. |
| `vstdlib/concommandhash.h` | 211 | **Deleted** — a hand-rolled open-addressing hash keyed by name. `HashMap<Box<str>, _>`. |
| `public/icvar.h` | 217 | **Deleted.** An `IAppSystem` with an iterator factory. |
| `public/vstdlib/cvar.h` | 25 | Deleted. |

### `engine/` — 5,483

| File | Lines | Disposition |
|---|---|---|
| `cvar.cpp` | 1,425 | `CCvarUtilities`: the set path (`IsCommand`, `:366`), `WriteVariables` (`:637`, config.cfg), and the `cvarlist`/`help`/`differences`/`toggle`/`findflags` commands. **Port the set path and the writer; the list commands are cheap and worth having.** |
| `console.cpp` | 1,652 | **Splits in three.** The print plumbing (`Con_ColorPrint`, `:767`) is the port target; the `con_logfile`/`con_filter_*` cvars come with it; `CConPanel` (the notify area, ~500 lines of VGui) goes to the HUD; the logging-channel commands (`log_level`, `log_flags`) are `tier0`'s and are deleted. |
| `cmd.cpp` | 1,171 | **The heart of the port.** `Cbuf_*`, `Cmd_ExecuteCommand` (`:929`) and its dispatch order, aliases, `exec` (`:500`), `stuffcmds` (`:357`). Roughly a third is splitscreen, execution markers and server forwarding, all of which go or defer. |
| `cmd.h` | 254 | Half of it is a **comment explaining the command sources** — read it before §4.7. |
| `netconsole.cpp` + `.h` | 415 | **Deleted.** A TCP listener that executes whatever it is sent. Unauthenticated remote command execution; nothing in scope wants it. |
| `ipc_console.cpp` | 294 | **Deleted by platform scope** — its first include is `<windows.h>`. |
| `cl_bounded_cvars.cpp` + `.h` | 186 | **Deferred to `net/`.** `cl_interp`, `cl_updaterate` and friends, clamped by server cvars. |
| `cheatcodes.cpp` + `.h` | 188 | **Deleted.** Konami-style key sequences read from `scripts/cheatcodes.txt`; **Portal 2 ships no such file** (checked against the depot). |
| `baseautocompletefilelist.cpp` + `.h` | 152 | **Port, shrunk.** "Complete this argument from files matching `cfg/*.cfg`" is what `exec` and `map` want. ~30 lines against the `Vfs`. |
| `cvar.h`, `console.h`, `gl_cvars.h` | ~200 | Dissolve. `gl_cvars.h:30`'s `CanCheat()` is three lines and survives as one. |

### UI — 1,806, all replaced by `egui`

`vgui2/vgui_controls/consoledialog.cpp` (1,371) + its header (182) — the dialog,
the history ring, the completion menu. `game/client/portal2/gameui/gameconsole.cpp`
(148) + `gameconsoledialog.cpp` (99) are the game-side wrapper that owns one.
**The algorithms in `RebuildCompletionList` (`:510`) and `CommandMatchesText`
(`:451`) survive** (§4.10); the widget tree does not.

### Not counted, deliberately

`dedicated/console/` (three `TextConsole` backends, ~900) — gated on
`ENGINE.md` open question 4. `tools/toolutils/ConsolePage.cpp`,
`utils/common/consolewnd.cpp`, `rocketui/customelements/KisakConvarSetting.cpp`,
the `gameui/cvar*` widgets — tools and UI, out of scope.

**~12,200 lines in scope; expect the Rust module to land around 2,000–2,500**,
with roughly 4,300 deleted outright (§5) and 1,800 replaced by `egui`.

---

## 3. Dependency graph

**Fan-in is the whole engine, which is the entire argument for porting it now.**

| Module | `ConVar` declarations | `CON_COMMAND*` |
|---|---:|---:|
| `game/` | 2,744 | 339 |
| `engine/` | 877 | 398 |
| `materialsystem/` | 263 | 17 |
| `filesystem/` | 26 | 20 |
| `inputsystem/` | 21 | 2 |
| everything else | ~239 | ~73 |

Fan-out is almost nothing, and that is the second reason to do it now:

- **`filesystem`** — `exec` reads `cfg/*.cfg`; the config writer writes one.
  Already ported.
- **`tier0`** — logging and `CommandLine()`. Both already answered:
  `eprintln!` today (§4.9), and `src/launcher/cmdline.rs`.
- **Nothing else.** `console/` does not need the renderer, the host, input, or a
  server. Its only *outbound* edge is the `CommandTarget` seam (§6.3), which is
  a trait it defines and someone else implements.

That asymmetry — everything depends on it, it depends on nothing — is why
`PORTING.md` puts it next, and why the module can be written and fully tested
before a single caller is converted.

---

## 4. The architecture you need in your head

### 4.1 One system, three modules, and why it is split that way

The split is an artifact of the DLL model and nothing else. `ConVar` lives in
`tier1` because *every* DLL needs the class; the registry lives in `vstdlib`
because exactly one process-wide instance must exist and `vstdlib` is the
lowest shared `.so`; the policy lives in `engine` because that is who knows
about servers, cheats and demos. **In one binary the three are one module**, and
`console/` is the whole of it.

The C++ dance that connects them — `ConVar_Register(flags, accessor)` walking a
static-init linked list and handing each entry to `g_pCVar` through an
`IConCommandBaseAccessor` — exists purely to get objects constructed in one `.so`
into a registry living in another. §4.8, §5.

### 4.2 The command buffer: text in, argv out, and `wait`

`CCommandBuffer` (`tier1/commandbuffer.cpp`) is small and every part of it is
load-bearing:

- **`AddText` splits text into commands and stores the text**, not the tokens
  (`:213`). Splitting is on `;` and `\n`, respecting quotes and `//` comments
  (`GetNextCommandLength`, `:162`). Tokenizing happens at *dequeue* time, which
  is what makes `wait` and delayed commands possible.
- **Commands carry a tick.** `AddText(text, source, nTickDelay)` schedules for
  `m_nCurrentTick + nTickDelay`, and the queue is kept sorted by tick
  (`InsertCommandAtAppropriateTime`). `BeginProcessingCommands(nDelta)` sets the
  window; `DequeueNextCommand` refuses anything past it.
- **`wait` is handled at insert time, not execute time** (`:236`). `AddText`
  recognises the literal token `wait`, adds its argument (default
  `m_nWaitDelayTicks`) to the running tick, and *drops the command* — the
  remaining commands in the same text get the later tick. This is why `wait` in
  a `.cfg` is a scheduling primitive rather than a sleep.
- **Insertion during processing goes to the head, not the tail**
  (`InsertImmediateCommand`, `:110`). An alias expanding to three commands runs
  those three *next*, not after everything already queued. Get this wrong and
  aliases execute in a plausible but wrong order.
- The engine "spoofs" ticks: `Cbuf_Execute` calls `BeginProcessingCommands(1)`
  every time (`cmd.cpp:288`), so a "tick" is one `Cbuf_Execute` call, not a
  server tick. **Keep that** — it makes `wait 1` mean "next frame", which is
  what the shipped `.cfg` files assume.

Buffer limits: `ARGS_BUFFER_LENGTH` 8,192 for the whole queue,
`COMMAND_MAX_LENGTH` 512 per command, `COMMAND_MAX_ARGC` 64. The fixed buffers
and the `Compact()` that services them are `Vec`/`String` in Rust — **drop the
limits**, but keep a cap on the queue so a runaway alias loop fails loudly
rather than eating memory.

### 4.3 Two tokenizers, not one

This is the easiest thing in the module to get subtly wrong, because the two
splitters disagree:

1. **`GetNextCommandLength`** (`commandbuffer.cpp:162`) splits *text into
   commands* on `;` and `\n`. It tracks quotes and `//` comments itself, and —
   flagged in Valve's own comment — **breaks on `\n` even inside a quoted
   string**.
2. **`CCommand::Tokenize`** (`convar.cpp:407`) splits *one command into argv*,
   via `CUtlBuffer::ParseToken` (`utlbuffer.cpp:1357`) with the break set
   `{}()':`. Quoted strings are one token with the quotes stripped; each break
   character is its own single-character token; `//` starts a comment;
   everything `<= ' '` separates.

Two details from `Tokenize` that look like noise and are not:

- **`m_nArgv0Size` and `ArgS()`.** `ArgS()` is "everything after argv[0], as
  typed" — which is how `cvarname "some value"` sets a string containing
  spaces without the tokenizer's quote handling getting in the way. The
  arithmetic at `convar.cpp:451-471` exists to make `"foo"bar` parse as two
  args with `ArgS()` pointing at `bar`. Port `ArgS` as "the raw remainder of
  the command string", and keep the test.
- **The set path re-strips quotes** (`cvar.cpp:481-514`): it takes `ArgS()`,
  drops a leading `"`, strips trailing whitespace, then drops a trailing `"`.
  So `hostname "  a b  "` keeps its interior spaces. That is the behavior to
  reproduce, not the char-pointer walk.

### 4.4 The dispatch order

`Cmd_ExecuteCommand` (`cmd.cpp:929`) tries, in this exact order:

1. **Execution markers** — `CMDSTR_ADD_EXECUTION_MARKER`, a nonce-guarded
   in-band signal used by `ClientCmd_Unrestricted`. **Deleted** (§5); revisit
   with `client/`.
2. **Aliases**, case-insensitively. A hit re-inserts the alias body at the head
   of the buffer and returns — **an alias is text substitution, not a call**, so
   it re-enters the whole dispatch and can itself expand.
3. **Commands**, by name. Then the gauntlet: `FCVAR_SERVER_CAN_EXECUTE` /
   `FCVAR_CLIENTCMD_CAN_EXECUTE` (§4.7), `FCVAR_GAMEDLL` forwarding to the
   server, `FCVAR_CHEAT` against `CanCheat()`, `FCVAR_SPONLY`,
   `FCVAR_DEVELOPMENTONLY`.
4. **Cvars**, via `CCvarUtilities::IsCommand` (`cvar.cpp:366`) — the name is a
   historical lie, it means "was this a cvar, and if so get or set it". One
   argument prints the description; more sets it.
5. **Forward to the server** if connected — which is how `kill` or `say` reach
   the game DLL.
6. **`Unknown command "%s"`.**

Order 2-before-3 matters: **an alias shadows a command of the same name.**
Order 3-before-4 matters less (no name is both) but is worth keeping so a
future duplicate resolves the way Valve's did.

For the port, steps 1, 3's forwarding, and 5 are absent, so the order is:
alias → command → cvar → unknown. **Write it as one function with that order
made explicit**, because it is the thing every future subsystem's commands
inherit.

### 4.5 `exec` is line-at-a-time and immediate

`_Cmd_Exec_f` (`cmd.cpp:500`) does **not** append the file to the buffer. It
reads one line, inserts it, and drains everything that line produced *before*
reading the next (`:604-635`). Consequences worth keeping:

- An `exec` inside a `.cfg` fully completes before the rest of the outer file
  runs. `valve.rc` depends on this.
- A `wait` inside an exec'd file does not do what a naive reading suggests.
- A syntax error on line 3 does not prevent lines 1-2 from having run.

Also in there, and worth reproducing:

- **Path is `//<pathid>/cfg/<name>`, defaulting to path ID `*`** — any mount.
  `exec config.cfg mod` restricts to the mod directory. `rustdocs/FILESYSTEM.md`'s
  `PathId` is the existing spelling of that argument.
- **`.cfg` is appended if absent**, and non-`.cfg`/`.rc` extensions are refused
  (`IsValidFileExtension`, `:439`) — this is a content-trust check, keep it.
- **Files over 1 MB are refused.**
- **`autoexec.cfg`, `joystick.cfg` and `game.cfg` fail silently** (`:572`). This
  looks like a hack and is exactly right: Portal 2 ships **neither**
  `autoexec.cfg` nor `joystick.cfg`, and `valve.rc` execs both. Without the
  special case, every launch prints two errors.

The shipped `portal2/cfg/valve.rc` — verified against the depot, and the whole
reason stage 1 is shaped the way it is:

```
exec joystick.cfg     // absent; must fail silently
exec autoexec.cfg     // absent; must fail silently
stuffcmds             // <- this is where +map takes effect
startupmenu           // GameUI; no-op for now
```

and `host.cpp:2055-2071` execs `config.cfg` (falling back to
`config_default.cfg`, which is the shipped default **bindings**) before it.

### 4.6 The flag set, flag by flag

`public/tier1/iconvar.h`. Bit values are fixed by shipped `.cfg` and by nothing
else — no content spells a flag — so the *numbers* are ours, but the *meanings*
are not.

| Flag | Bit | Disposition |
|---|---:|---|
| `DEVELOPMENTONLY` | 1 | **Keep.** Hidden and unsettable in release builds. |
| `HIDDEN` | 4 | **Keep.** Like the above but not compiled out. |
| `ARCHIVE` | 7 | **Keep.** Load-bearing: it is what `config.cfg` is. |
| `NEVER_AS_STRING` | 12 | **Keep.** Changes both the set path and completion display. |
| `CHEAT` | 14 | **Keep.** Gated on `sv_cheats` via `CanCheat()` (`gl_cvars.h:30`). Portal 2 uses it heavily. |
| `SPONLY` | 6 | **Keep** as a no-op predicate — everything is single-player until `server/`. |
| `RELOAD_MATERIALS` / `RELOAD_TEXTURES` | 20, 21 | **Replace** with the generation counter (§6.2): the material system notices a change rather than being told. |
| `PROTECTED`, `NOTIFY`, `USERINFO`, `PRINTABLEONLY`, `UNLOGGED`, `REPLICATED`, `NOT_CONNECTED`, `SERVER_CAN_EXECUTE`, `SERVER_CANNOT_QUERY`, `CLIENTCMD_CAN_EXECUTE` | 5, 8, 9, 10, 11, 13, 22, 28, 29, 30 | **Defer to `net/`/`server/`.** These are the untrusted-source model (§4.7). Leave the bits reserved and unimplemented rather than inventing something now. |
| `GAMEDLL`, `CLIENTDLL` | 2, 3 | **Deferred, renamed.** They mean "server-side"/"client-side", which still exists as a concept in a listen server; they just are not DLLs. Do not reintroduce them before `server/` needs the distinction. |
| `DEMO`, `DONTRECORD` | 16, 17 | **Defer** with `demo/`. |
| `UNREGISTERED` | 0 | **Delete.** Registration is explicit. |
| `SS`, `SS_ADDED` | 15, 18 | **Delete.** Splitscreen (§5). |
| `MATERIAL_SYSTEM_THREAD`, `ACCESSIBLE_FROM_THREADS` | 23, 25 | **Delete.** Their whole purpose is `CCvar::QueueMaterialThreadSetValue` (`vstdlib/cvar.cpp:774`), a deferred-write queue for a cvar read off-thread. An atomic cell (§6.2) makes the problem not exist. |
| `ARCHIVE_GAMECONSOLE` | 24 | **Delete.** X360/PS3. |
| `RELEASE` | 19 | **Delete.** A CS:GO-era "customer-visible" allowlist. |
| `SERVER_CAN_EXECUTE`… (see above) | 28-30 | — |

**The trap:** `FCVAR_PRINTABLEONLY` and `FCVAR_GAMEDLL_FOR_REMOTE_CLIENTS` are
**both `1<<10`**. It is deliberate overloading — bit 10 means one thing on a
`ConVar` and another on a `ConCommand` — and it is exactly the kind of thing a
faithful transliteration reproduces by accident. In Rust, a cvar and a command
do not share a flag type; give them separate flag sets and the collision cannot
be expressed.

### 4.7 `cmd_source_t` is a security model

`convar.h:88` and the 100-line comment at the top of `cmd.h`. Every command
carries where it came from: `kCommandSrcCode`, `kCommandSrcClientCmd`,
`kCommandSrcUserInput`, `kCommandSrcNetClient`, `kCommandSrcNetServer`,
`kCommandSrcDemoFile`. The flags in §4.6 are read *against* that source — a
`clc_stringcmd` from a connected client may only run `FCVAR_GAMEDLL` commands, a
server may only make its client run `FCVAR_SERVER_CAN_EXECUTE` ones.

**Port the enum now, even though only two variants are reachable.** Two reasons:
it is one enum and costs nothing; and when `net/` lands, the alternative is
retrofitting a provenance field through a dispatcher that has been assuming
trust — which is how this class of bug ships. Valve's own comment on
`kCommandSrcDemoFile` ("*Should be heavily restricted as demo commands can come
from untrusted sources*") is the warning label.

Today: `Code` (startup, the engine's own `Cbuf_AddText`) and `UserInput` (a
keybind, and later the console line). Everything else is `unimplemented` with a
comment naming the module that will produce it.

### 4.8 Registration, and the linkage that only exists because of DLLs

Valve's registration is a **static-initializer linked list**: every `ConVar` is
a file-scope global whose constructor pushes it onto `s_pConCommandBases`
(`convar.cpp:157`), and `ConVar_Register` later walks that list handing each
entry to the one true registry through an accessor
(`convar.cpp:56`, `vstdlib/cvar.cpp:347`).

Rust has no static constructors, and that is a feature here. But the more
interesting half is what `RegisterConCommand` does when a name is *already*
taken (`vstdlib/cvar.cpp:361-450`): it does not reject the duplicate — it
**links the newcomer as a child of the existing one**, transferring the child's
change callbacks and help text to the parent and having both objects read the
parent's value. That machinery, ~90 lines plus `m_pParent` on every access
(`convar.h:552-630`), exists for exactly one reason: `sv_cheats` is declared in
`engine`, `client.so` and `server.so`, and all three must see one value.

**In a single binary that reason is gone.** A duplicate name is a bug; report it
and refuse. This is the single largest deletion in the module and it takes
`ConVarRef`, `CVarDLLIdentifier_t`, `UnregisterConCommands(id)` and
`IConCommandBaseAccessor` with it.

One behavior from `ConVar::Create` is worth keeping: **the command line seeds
cvar defaults.** `CCvar::GetCommandLineValue` (`vstdlib/cvar.cpp:699`) looks up
`+<name>` in the process arguments. That is *not* `stuffcmds` — it is a distinct
path, and it is why `+fps_max 60` works before `valve.rc` runs. The port already
reads `+map` and `+fps_max` this way in `src/launcher/mod.rs:116-133`, with a
comment saying it is standing in for the command buffer; **that block is what
stage 1 deletes.**

### 4.9 Printing

`Con_ColorPrint` (`console.cpp:767`) is the funnel, and it fans out to six
places: the net console, the VGui console, the notify area, the debugger, the
`con_logfile` file, and `tier0`'s spew. Around it:

- **`con_filter_enable` / `con_filter_text` / `con_filter_text_out`** — substring
  include/exclude, with mode 2 dimming rather than dropping. Cheap and genuinely
  useful when a subsystem is spamming; **keep**.
- **`developer`** gates `Con_DPrintf`. Keep, as a level rather than a bool.
- **`Con_NPrintf`/`Con_NXPrintf`** (`public/con_nprint.h`) — the fixed-position
  overlay lines used for per-frame debug readouts. **Defer** to the HUD.
- **`SV_RedirectActive`** — RCON output capture. Deleted with RCON.

Today the port prints with `eprintln!("source-engine: <subsystem>: …")` in about
forty places. **Stage 1 does not convert them**; it adds the sink and uses it for
console output, and conversion happens per-subsystem when each grows a cvar
anyway. §9 open question 3 covers whether `tracing` should eventually own this.

### 4.10 Autocomplete

`RebuildCompletionList` (`consoledialog.cpp:510`). The rules, which are worth
keeping even though the widget is not:

- **Empty input lists history**, not everything.
- **Input containing a space** first asks whether a registered command claims
  argument completion for it (`FindAutoCompleteCommmandFromPartial` →
  `ConCommand::AutoCompleteSuggest`). `exec ` listing `cfg/*.cfg` is that path,
  and `map ` listing `maps/*.bsp` is the one worth adding.
- **Otherwise, prefix match**; if there was a space and no command claimed it,
  fall back to **space-separated substring matching** (`CommandMatchesText`,
  `:451`) — `"draw wire"` matches `mat_wireframe`. Non-obvious and pleasant.
- `FCVAR_DEVELOPMENTONLY` and `FCVAR_HIDDEN` are excluded.
- Cvars display their current value beside the name; `FCVAR_NEVER_AS_STRING`
  ones display a formatted number instead.
- Results are sorted by name; the cap is `COMMAND_COMPLETION_MAXITEMS` = 64.

Completion is **data on the `CommandSpec`** in the Rust design (§6.4), not a
callback: `Completion::None`, `Completion::Files { dir, ext }`, or
`Completion::Values(&[&str])`. That covers `exec`, `map` and the toggles without
a single closure, and the UI stage can add a dynamic variant if something needs
one.

---

## 5. What is deleted, and why

| Deleted | ~Lines | Why |
|---|---:|---|
| Cross-DLL parent/child cvar linkage, `IConCommandBaseAccessor`, `ConVar_Register`, `CVarDLLIdentifier_t` | ~400 | One binary. §4.8. |
| `ICvar`, `ICvarQuery`, the `IAppSystem` lifecycle, the iterator factory | ~500 | `PORTING.md`: no app system, no `CreateInterface`. |
| `CConCommandHash` | 211 | `HashMap`. |
| Splitscreen: `CBUF_COUNT` buffers, `FCVAR_SS`, `CSplitScreenAddedConVar`, `SplitScreenConVarRef`, `AddSplitScreenConVars`, `cmd1`…`cmd4` | ~600 | Out of scope. §9 open question 5 records the reversal cost. |
| Material-thread queued sets (`QueueMaterialThreadSetValue`, `ProcessQueuedMaterialThreadConVarSets`, `IsMaterialThreadSetAllowed`) | ~150 | Atomics. §4.6. |
| `netconsole.cpp`, `ipc_console.cpp` | 709 | Remote execution; and one is Windows-only. |
| `cheatcodes.cpp` | 188 | No `scripts/cheatcodes.txt` in Portal 2. |
| Execution markers (`CMDSTR_ADD_EXECUTION_MARKER`, `HandleExecutionMarker`, `g_ExecutionMarkers`) | ~90 | Serves `ClientCmd_Unrestricted`, which does not exist. |
| RCON redirect, Steam Cloud config write, `whitelistcmd`/`execwithwhitelist`, forwarded-command rate limiting | ~400 | Multiplayer/Steam; defer or delete. |
| `characterset_t`, `CUtlBuffer` parsing, fixed `char[512]` buffers, `CopyString` | ~300 | `str`, `String`, `Vec`. |
| `CConPanel` (the notify area) | ~500 | Goes to the HUD, not here. |
| The VGui dialog | 1,553 | `egui`. |
| `log_level`/`log_flags`/`log_color`/`log_dumpchannels` | ~250 | `tier0`'s logging channels; replaced wholesale. |

**~4,300 lines deleted outright**, ~1,800 replaced by `egui`.

---

## 6. The Rust design

### 6.1 Where the state lives — the `OnceLock` question, answered "no"

`ENGINE.md` §7.4 called the cvar registry "the one piece of ambient global state
that is genuinely process-global." **On inspection it is not**, and the
distinction is worth stating precisely because it is the design.

What is genuinely shared is **each cvar's value**, not the *registry*. The
registry is consulted by exactly one caller — the dispatcher, resolving a typed
name — and the dispatcher already has `&mut Console`. Everyone else wants one
specific cvar, forever, from the moment they are constructed.

So:

```rust
// Registration hands back a handle. The console keeps one, the caller keeps one.
let fps_max = console.cvar("fps_max", "300", Flags::ARCHIVE, "Frame rate limiter");
```

`Cvar` is `Arc<CvarCell>`. Reading is an atomic load through the caller's own
handle — no borrow of the console, no lock, no lookup, callable from any thread.
Writing goes through either end and every reader sees it. **The material system
does not need a `&Console` to read `mat_luxels`; it needs the `Cvar` it asked for
at construction.**

Three things fall out, all of them good:

- The `FCVAR_MATERIAL_SYSTEM_THREAD` queue (§4.6) has nothing to solve.
- `console/` stays a leaf module: it can be constructed, driven and asserted on
  in a unit test with no engine at all, which is what `host/` demonstrated is
  worth having.
- There is no initialization-order question, because there is no global to
  initialize.

The cost is that a cvar's *storage* outlives its registration if the holder
outlives the console — which is fine, and is exactly what an `Arc` is for.

**Considered and rejected:** a `OnceLock<Cvars>` with name lookup at every read
(a `HashMap` probe in a draw loop, and a global besides); and an
index-handle-into-a-`Vec` registry (forces `&Console` into every reader's
signature, which is the ambient-parameter version of the ambient global).

### 6.2 A cvar is a shared cell

Keep Valve's **triple cache**: `ConVar` stores the string, the float and the int,
recomputing all three on every set (`convar.cpp:848-1065`), so `GetFloat` in a
hot loop is a load. That is the right design and it maps onto atomics directly:

```rust
struct CvarCell {
    name: Box<str>,
    help: Box<str>,
    default: Box<str>,
    flags: CvarFlags,
    bounds: Option<(f32, f32)>,   // ConVar::ClampValue
    float: AtomicU32,             // f32::to_bits
    int: AtomicI32,
    string: RwLock<Arc<str>>,     // cheap to clone out; rarely read
    generation: AtomicU32,        // bumped on every change
}
```

- **`generation` replaces change callbacks.** A consumer that must react to a
  change compares the counter it last saw. That covers `RELOAD_MATERIALS`,
  `RELOAD_TEXTURES` and `fps_max` without storing a `Box<dyn Fn>` that would
  need `&mut` state it cannot have. **Do not port `FnChangeCallback_t`** unless
  something genuinely cannot poll — `con_logfile` opening a file is the one
  plausible case, and it is `console/`'s own and can be special-cased there.
- **Clamping**: `ClampValue` (`convar.cpp:945`) applies min/max on every set,
  including the initial one. Keep it, keep it before the string is formatted,
  and note that Valve clamps the *float* and reformats — so `fps_max -1` with
  min 0 stores `"0"`, not `"-1"`.
- **`Revert`** restores the default; `RevertFlaggedConVars(FCVAR_CHEAT)` is what
  `sv_cheats 0` triggers. Keep both.

### 6.3 A command is not a callback

`ConCommand` holds a function pointer that finds its state through globals.
Neither half survives. Instead, **declare as data, execute where the state is**:

```rust
pub struct CommandSpec {
    pub name: &'static str,
    pub help: &'static str,
    pub flags: CommandFlags,
    pub completion: Completion,
}

pub trait CommandTarget {
    /// Run one resolved command. `cx` is how a command queues more text
    /// (`exec`, `alias`) or prints.
    fn execute(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) -> Dispatch;
}
```

`Console::run(&mut self, target: &mut dyn CommandTarget)` is `Cbuf_Execute`: it
dequeues, resolves aliases and cvars itself, and calls `target.execute` for
anything else. `Dispatch::{Handled, Unknown}` is what produces
`Unknown command "%s"`.

This is deliberately the same shape as `host::Level`, and for the same reason
`ENGINE_INPUT.md` §8.3 gives for `CommandSink`: the module that is ready does
not wait for the modules that are not.

**`console/`'s own commands** (`exec`, `alias`, `echo`, `wait`, `clear`,
`cvarlist`, `help`, `find`, `toggle`, `incrementvar`) are handled inside the
console before the target is consulted — they need nothing else, and it keeps
the engine's `execute` down to the handful of commands that are genuinely the
engine's.

### 6.4 Types

Sketch, not a signature list — the API doc is written with the code.

```rust
pub struct Console { cvars: CvarRegistry, aliases: HashMap<..>, buffer: CommandBuffer, log: Log, commands: Vec<CommandSpec> }

pub struct Cvar(Arc<CvarCell>);      // §6.2; Clone, Send, Sync
pub struct Command<'a> { name: &'a str, args: &'a [&'a str], tail: &'a str, source: Source }
pub enum Source { Code, UserInput }  // §4.7; the rest arrive with net/
pub struct ExecContext<'a> { buffer: &'a mut CommandBuffer, log: &'a mut Log }
pub enum Completion { None, Files { dir: &'static str, ext: &'static str }, Values(&'static [&'static str]) }
```

`Command::tail` is `ArgS()` (§4.3) and is what the cvar set path uses.

### 6.5 The seams

**To `input/` — `CommandSink`, already specified.** `ENGINE_INPUT.md` §8.3
defines it and stage 3 of that plan is waiting on it. `Console` implements it;
`enqueue(&str)` is `Cbuf_AddText` with `Source::UserInput`. Keep the `+`/`-`
convention *and the button index argument* exactly (`keys.cpp:1148-1170`): a
binding starting with `+` sends `+forward <index>` on press and `-forward
<index>` on release. Note also `bind_osx` (`keys.cpp:333`) — Portal 2's shipped
`config_default.cfg` uses it, and macOS is a supported target, so it is a real
command and not a curiosity.

**To `host/` — the four commands that motivated this.** `map`, `quit`,
`restart`, `fps_max`. Three are `Host` methods that already exist
(`request_new_game`, `request_shutdown`, `request_restart`); the fourth is
`FrameClock::set_fps_max`, which becomes "read the cvar's generation each
frame."

**To `materials/` — the cvar handle, and nothing else.** `mat_wireframe`,
`mat_luxels`, `mat_fullbright` are `Cvar`s the material system holds. No
`&Console` reaches the renderer.

**To `egui` — later.** The console UI reads `Log`'s ring and calls
`Console::enqueue`; completion asks `Console::complete(&str) -> Vec<Completion>`.
Both are pure functions of console state, which is what keeps the UI stage from
touching anything else.

**To the launcher — deleting a wart.** `src/launcher/mod.rs`'s `+map`/`+fps_max`
block (§4.8) is replaced by `stuffcmds`. `CLAUDE.md`'s recorded wart —
"`CommandLine` lives in `src/launcher/` but is read from `src/engine/window/`;
move it to `src/cmdline.rs` when a third subsystem needs it" — **is triggered
by this port**: `console/` is that third subsystem, since `stuffcmds` and the
`+<cvar>` default seeding both read the command line. Do the move as part of
stage 1.

### 6.6 The borrow that will bite

`Console` will be a field of `Engine`, and `Console::run(&mut self, target)`
needs a target that is *also* made of `Engine`'s fields. `self.console.run(&mut
self)` does not compile, and the fix is the one `Engine::frame` already uses for
`host.frame(&mut self.scene)`: pass a struct of disjoint `&mut` field borrows.

```rust
let Engine { console, host, scene, input } = self;
console.run(&mut EngineCommands { host, scene, input });
```

Two consequences to design for rather than discover:

- **`Engine` cannot be the `CommandTarget` itself**; a small struct of borrows
  is, and it is constructed per call. Cheap, and it makes the set of state a
  command may touch explicit — which is a genuine improvement over the C++,
  where the answer was "all of it."
- **Re-entrancy goes through `ExecContext`, not through `&mut Console`.** A
  command that queues more text (`exec`, `alias`) writes into
  `cx.buffer`, which is a field borrow of the console the dispatcher is already
  inside. That is why the context exists; without it, `exec` cannot compile.

---

## 7. Fixed formats: what is external content

`PORTING.md`'s "format is fixed" rule applies to more of this module than it
first appears. All of the following are read from Valve's shipped content and
are **not** ours to change:

- **`cfg/*.cfg` and `cfg/valve.rc` syntax** — `;` and newline separation, `//`
  comments, quoted arguments, `wait`, `exec`. `portal2/cfg/` ships ~60 of them,
  including `config_default.cfg` (the default bindings) and the `chapter*.cfg`
  files.
- **Cvar and command *names*** — every one that shipped content mentions.
  `modsettings.cfg` alone sets `mat_bloom_scalefactor_scalar`,
  `voice_local_icon`, `voice_all_icons` and `sys_minidumpexpandedspew`; an
  unrecognized name there is an error message on every launch.
- **`scripts/kb_def.lst`** — the default binding table, parsed as KeyValues by
  `GetDefaultKeyBindings` (`keys.cpp:348`). Already `filesystem/`'s reader.
- **`config.cfg`'s output format** — `unbindall`, then bindings, then
  `<name> "<value>"` per archived cvar (`host.cpp:1624`, `cvar.cpp:637`). It is
  read back by `exec`, so producer and consumer are both ours — but a user's
  existing `config.cfg` is not, and writing a format the shipped `exec` cannot
  read is a data-loss bug the first time someone points the port at a real
  Steam install.

**What is ours:** the flag *bit values* (§4.6), the in-memory representation,
the ring buffer, and anything the console prints.

---

## 8. Staged plan

Each stage is independently reviewable and independently useful.

1. **Cvars, commands, the buffer, and `exec`.** `Cvar`/`CvarCell`, the registry,
   `CommandBuffer` with ticks and `wait`, both tokenizers, the dispatch order,
   aliases, `exec`, `echo`, `stuffcmds`, and the `Log` sink. `CommandTarget`
   implemented by `Engine` for `map`/`quit`/`restart`, and `fps_max` as the first
   real cvar. `CommandLine` moves to `src/cmdline.rs`.
   *Deliverable:* `cargo run -- -basedir … -game portal2 +map sp_a1_intro1`
   loads the map **through `exec valve.rc` → `stuffcmds`**, and
   `src/launcher/mod.rs`'s `+map` block is deleted.
   *Tests:* the two splitters against quoted/commented/`;`-joined text; `wait`
   scheduling; alias expansion inserting at the head; alias shadowing a command;
   `exec` running line-at-a-time; the cvar set path's quote stripping; clamping;
   a duplicate registration being an error (§4.8).
   *Not here:* bindings, the UI, config writing.

2. **Bindings.** `ENGINE_INPUT.md` stage 3 — the binding table, `+`/`-` with the
   index argument, `bind`/`bind_osx`/`unbind`/`unbindall`, and `kb_def.lst`
   defaults. `console/` supplies `CommandSink`; the table itself lives in
   `input/`.
   *Deliverable:* WASD comes from `config_default.cfg` instead of from
   `FlyCamera`'s hard-coded keys.

3. **Config persistence.** `FCVAR_ARCHIVE`, `Host_WriteConfiguration`
   (`host.cpp:1559`) minus Steam Cloud, `Key_WriteBindings`, and the
   `config.cfg` → `config_default.cfg` fallback at startup. **Guard it the way
   Valve does:** do not write a config until one has been read
   (`Host_WasConfigCfgExecuted`), or a crashed first launch overwrites a real
   user's settings with defaults.

4. **The dev console UI.** *Wants `egui`*, which is unscheduled. The ring, the
   input line, history, the completion popup and `toggleconsole` — plus
   `ENGINE_INPUT.md` stage 4's UI precedence and key-up latch, which lands with
   the same integration and is the reason the two stages should be done
   together.

5. **The list/diagnostic commands.** `cvarlist`, `help`, `find`, `differences`,
   `toggle`, `incrementvar`. Trivial once 1 and 4 exist, useless before.

Stages 1 and 2 are what "port `console/`" means for the boot path. 3 is small
and can follow immediately; 4 is gated on a decision that is not this module's.

---

## 9. Open questions and risks

1. **Is `Arc<CvarCell>` right, or is it one indirection too clever?** §6.1
   argues yes and the argument is about *where reads happen*, which is a fact
   about this port rather than a preference. But it is the decision the whole
   module hangs off, and it is worth a second look while writing stage 1's first
   consumer (`fps_max`). The fallback — a `Console`-owned registry with index
   handles — is a mechanical change if `Arc` turns out to be wrong, provided
   callers only ever hold `Cvar` and never a `&CvarCell`. **Keep that
   invariant.**
2. **Does the `generation` counter actually replace change callbacks?** It works
   for pollers. It does not work for a consumer that must act *at the moment* of
   the change and never runs otherwise. No such consumer exists in the port
   today; `con_logfile` is the first plausible one and is `console/`'s own.
   Revisit when something outside `console/` needs it.
3. **`tracing`, or a hand-rolled sink?** `PORTING.md` names a logging crate as
   `tier0`'s replacement, and `developer 1/2` plus `con_filter_*` are
   recognisably level-and-target filtering. Against: the port has ~40
   `eprintln!` sites and no other logging need yet, and a `tracing` `Layer`
   feeding the console ring is more machinery than a `Vec<Line>`. **Decision for
   now: hand-rolled.** **Trigger for revisiting: the second consumer of
   structured output** — a log file with levels, or per-subsystem filtering that
   `con_filter_text` cannot express.
4. **How much of `cmd_source_t` to build now.** §4.7 says port the enum. It
   does not say to build the flag-vs-source permission matrix, which cannot be
   tested without a network. The risk is that the matrix arrives late and gets
   bolted onto a dispatcher that has been assuming trust. Mitigation: put the
   check in the dispatcher *now* as a function that currently returns `Allowed`
   for everything, so `net/` fills in a body rather than finding a seam.
5. **Splitscreen.** Deleted (§5), and Portal 2 does ship splitscreen co-op.
   Reversing it means: per-target command buffers, `FCVAR_SS`'s `varname2`
   auto-generation, and `cmd1`…`cmd4`. The cost is contained to `console/` and
   `input/` — **recorded so the deletion is not mistaken for an oversight.**
6. **Does anything shipped depend on a cvar the port does not have?**
   `modsettings.cfg` and `config_default.cfg` are exec'd unconditionally at
   startup and reference cvars from subsystems that do not exist. Unknown names
   must be a *quiet* message, not a wall of errors — but silence hides typos.
   Suggested: count them and print one summary line, which is also a decent
   progress metric for the port.

---

## 10. Notes for whoever picks this up

- **The graph cannot see `CON_COMMAND`.** `check_index_coverage` reports
  `parse_partial` on exactly the macro invocation lines — `cmd.cpp` 137, 357,
  681, 698, 772, 792; `cvar.cpp` 1290, 1299, 1308, 1316, 1324, 1353, 1362 — because
  the macro body is what declares the function. So `search_graph` under-reports
  console commands specifically, which is the worst possible blind spot for this
  module. **Find commands by grepping `CON_COMMAND`**, not by asking the graph.
  Also unparsed: `vstdlib/cvar.cpp:219-373`, which is most of `CCvar`'s class
  body and all of `RegisterConCommand`'s opening. `tier1/convar.cpp`,
  `tier1/commandbuffer.cpp` and `consoledialog.cpp` are clean.
- **`legacy/` is latin-1.** Shell `grep` without `-a` returns nothing from these
  files. See `CLAUDE.md`'s "Searching" section — this module's files are among
  the affected ones.
- **Read the comment block at the top of `engine/cmd.h`** before §4.7. It is 100
  lines of Valve explaining their own command-source model, and it is the best
  documentation of it that exists.
- **Verify against the shipped content, not just the source.** The Portal 2
  depot is the arbiter for §7: `portal2/cfg/valve.rc` is fourteen lines and
  determines stage 1's entire scope, and the absence of `autoexec.cfg` and
  `joystick.cfg` is what makes `cmd.cpp:572`'s odd special case correct.
- Line numbers are from the tree at time of writing; re-verify before relying on
  them. Sizes in §2 were taken with `wc -l` against the current tree.
- Only POSIX paths are documented, per `PORTING.md`. `ipc_console.cpp` is the
  only file here that is Windows-only outright, but `console.cpp` and `cmd.cpp`
  are dense with `_X360`/`_PS3`/`_GAMECONSOLE` branching — skim for unconditional
  code and disregard the rest.
