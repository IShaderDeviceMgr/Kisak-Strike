# VScript — porting notes

> **Written with the port, not before it.** `portdocs/SERVER.md` §9 left VScript
> an open question and asked for one measurement first. That measurement, and the
> decision it led to, are here. Read this as the analysis that justifies the shape
> of `src/vscript/` and `src/server/script.rs`, not as a plan still to follow.
> `rustdocs/VSCRIPT.md` is the API.

## 1. What VScript is in Portal 2

Portal 2's maps are scripted in **Squirrel 2.2.3** (`legacy/vscript/languages/squirrel/`,
`HISTORY` tops out at 2.2.3 even though `readme_valve.txt` says 2.1), through
`vsquirrel.cpp`'s `CSquirrelVM` and the game's bindings in `vscript_server.cpp`,
`vscript_shared.cpp` and `baseentity.cpp`.

The measurements that sized it, all over the 106 shipped maps and the depot's
`portal2/scripts/vscripts/`:

| | |
|---|---|
| Script files | **92**, **64,067 lines**, all loose (none is in a VPK) |
| Entities with a `vscripts` key | **681** on 105 maps — `logic_script` 384, `generic_actor` 111, `point_template` 45, `func_tracktrain` 42, `trigger_teleport` 37, `prop_testchamber_door` 33, … |
| Entities with a `thinkfunction` | **295** |
| Connections that fire a script input | **2,267** (`RunScriptCode`, `CallScriptFunction`, `RunScriptFile`) — 516 distinct calls |
| Commonest calls | `ReadyForTransition()` 459, `TransitionFromMap()` 145, `GladosCoopElevatorEntrance()` 94, `GladosCoopOpenExitDoor()` 76, `DisplayChapterTitle()` 62, `OnPostTransition()` 58, … |

**Every step of an elevator is a script call.** Arrival is `OnPostTransition()` in
`transitions/sp_transition_list.nut` (it fires `@arrival_teleport`); departure is
`StartMoving()` in `sp_elevator_motifs.nut` (it fires `SetSpeedReal` with a per-map
speed from a table), then `ReadyForTransition()`/`FailSafeTransition()` from the path's
`OnPass`, then `TransitionFromMap()`, which looks the next map up in `MapPlayOrder`.
Without a VM, no map in the game can be left or entered the way it was designed.

## 2. The decision

`portdocs/SERVER.md` §9 listed four options: defer, a crate, FFI to `libsquirrel`, or
reimplement the scripts' call surface in Rust. **The one taken is a fifth: write the
VM.** Rewriting the half-dozen transition functions in Rust (option 4) was judged too
great a divergence — it replaces Valve's *content* with the port's reading of it — and
no pure-Rust Squirrel 2 exists to take as a crate.

What "write the VM" means, precisely:

- **The language is Squirrel 2.2.3's, rule for rule** — every compile decision
  (`sqcompiler.cpp`) and every runtime rule (`sqvm.cpp`) is reproduced, including the
  ones that are not what a reader expects (§4). The test of it is that all 92 shipped
  scripts compile and that the scripts' own output matches what the game printed.
- **The executor is a tree walker, not a port of the bytecode VM.** The register
  bytecode is an encoding; what carries across is what it decides. The AST keeps every
  distinction that changes an answer — a method call against a grouped call, a local
  against a free variable against a constant against a field of `this`.
- **Valve's binding layer is ported with it**: `CSquirrelVM::Init`'s standard libraries,
  the `Vector` class, `init.nut` and `vscript_server.nut` byte for byte, entity scopes as
  tables in the root table, `self` as a class instance, the `Input<name>` hook, script
  thinks, `EntityGroup`.

## 3. Numbers that decided the representation

- **32-bit integers and 32-bit floats.** `_SQ64` is defined only for 64-bit builds,
  and `SQUSEDOUBLE` comes with it; Portal 2 shipped 32-bit. Integer arithmetic wraps; a
  float that overflows an integer conversion is `0x80000000`, x86's "integer
  indefinite".
- **Strings are bytes.** A `.nut` file is `char *`; `init.nut` itself has a Latin-1 `©`.
- **Tables are Lua 4.0's hash, replicated.** `foreach` walks the node array, so the
  order a script sees its keys in is the table's layout. Keys that hash by value
  (strings, numbers, bools) come out in Valve's order; keys that hash by address cannot,
  in either program.
- **One VM per level.** `CVScriptGameSystem` makes it at `LevelInitPreEntity` and
  destroys it at `LevelShutdownPostEntity`; `::TransitionFired` does not survive a map
  change, and the transition scripts rely on that.

## 4. What the C does that a reader would not guess

Each of these is reproduced, and most are pinned by a test in `src/vscript/tests.rs`.

1. **No upvalues.** A function sees an enclosing local only if it names it in
   `function(...):(x)`, and then it gets a *copy* taken when the closure is made.
   Anything else unqualified is `this.x`.
2. **An unqualified name falls back to the root table only when the object is the
   running function's own `this`** (`SQVM::Get`'s `fetchroot`). That rule, plus an entity
   scope's delegate being the root table, is how a script in an entity's scope reaches a
   global.
3. **`==` and `<` share one precedence level**, `in` and `instanceof` sit between `&&`
   and `|`, and unary operators bind to a whole postfix expression (`-a.b` is `-(a.b)`).
4. **A newline ends a statement**, blocks an index (`a\n[0]` is a compile error,
   "cannot brake deref"), and does **not** block a call (`f\n(1)` calls `f`).
5. **The top-level compile loop has no `;` exception after a `}`.** `local t = {}; x()`
   on one line is a compile error at file scope and fine inside a block, because
   `SQCompiler::Compile` checks `_prevtoken != '}'` where `Statements()` also checks `';'`.
6. **`1.5.tostring()` does not parse**: `ReadNumber` takes every `.` it meets and
   `strtod` quietly stops at the second.
7. **Floats compare equal by bits** (`_rawval`), so `0.0 != -0.0` and NaN equals itself;
   two floats of the same type never compare *equal* in `<=` unless their bits match.
8. **An error reaches the error handler when it is raised, and only if the current
   `Execute` has no `try` open** — so an error inside an `array.sort` comparator is
   printed even when the sort is inside a `try`, because the comparator runs in a new
   `Execute`.
9. **Valve appends `_lasterror` to arithmetic errors** ("arith op + on between 'bool'
   and 'integer' (…)"), and `_lasterror` persists across caught errors.
10. **An `Input<name>` hook that returns nothing swallows the input** —
    `functionReturn.m_bool` reads the union's low byte, and a void return is zero.
11. **`GetScriptId()` returns the think function's name**, `GetLeftVector()` returns the
    *right* vector, `ToKVString()` ends in `))`, and `logic_script`'s sixteenth group key
    is `Group16`. All Valve's, all kept.

## 5. What is not ported, and why

| | Why | Measured |
|---|---|---|
| `yield`, `resume`, generators, `newthread`, `suspend` | A tree walker cannot suspend without a second design | **0** of the 92 shipped scripts use any of them |
| The remote debugger (`sqdbg`) | Tooling | — |
| Saving and restoring VM state | There are no saves | — |
| `CreateSceneEntity` | Needs the choreographed-scene system | 4,647 of the scripts' calls; stops `turret_vo_manager.nut` and `credits_coop.nut` at load |
| `TraceLine`, `CreateProp`, `CreateByClassname`, `SetModel`, `GetSoundDuration`, voice, HUD, Steam, the portal gun | Each needs a system that is not here | — |
| Script classes for `CBaseAnimating`, `CBaseFlex`, `CSceneEntity`, … | Their members need animation and choreography from script | The player has `CBasePlayer`; everything else is `CBaseEntity` |

The rule for an absent native is **not to register it**: a script that calls one fails
with Squirrel's own "the index 'X' does not exist", which is honest and loud, rather than
getting a stub that returns something plausible.

## 6. What it bought

Measured by `server::tests::every_shipped_maps_scripts_run`: every shipped map, loaded
with the game's scripts and a player, run for five seconds. **104 of the 106 raise no
error.** The two that do are the same absence, `CreateSceneEntity`, called while a file
loads.

Measured by `server::tests::sp_a1_intro2s_elevators_run_on_the_maps_own_scripts`:
`sp_a1_intro2` is entered and left the way it was designed, on its own scripts —
`OnPostTransition()` teleports the player into the arrival car, the train descends,
`StartMoving()` sends the exit car down at the 200 units/s the motif table gives this
map with the player riding its floor for 4,250 units, `FailSafeTransition()` fires at
the bottom, and `TransitionFromMap()` fires `Changelevel` at `@changelevel`, whose
`point_changelevel` asks the engine for `changelevel sp_a1_intro3`.

When VScript first landed that last step took the script's **own fallback** —
`@changelevel` did not exist, so the script sent `map <next>` to the console. Porting
`point_changelevel` (`rustdocs/SERVER.md`, "`point_changelevel` — leaving a map") put
the designed path back, and the depot test now asserts the fallback is not taken.
