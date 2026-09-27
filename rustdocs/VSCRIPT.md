# `src/vscript/` — Squirrel 2.2 (API reference)

Portal 2's scripting language, as a VM the server owns one of per level. The
language half is here; the game's bindings (`EntFire`, `Entities`, `self`) are
`src/server/script.rs` and are documented in `rustdocs/SERVER.md`, "VScript — the
server's half". Why it is a VM and not a rewrite of the scripts, and every measurement
that shaped it, is `portdocs/VSCRIPT.md`.

| | |
|---|---|
| Replaces | `legacy/vscript/languages/squirrel/` (Squirrel 2.2.3, 10,531 lines of core and `sqstdlib`) and the language half of `vsquirrel/vsquirrel.cpp` (3,548) |
| Size | 7,950 lines, 460 of them tests |
| Language | Complete **except** `yield`/`resume`/generators and threads, which no shipped script uses |
| Standard library | The base library, every default delegate, `sqstdmath`, `sqstdstring` including its `regexp`, the standard error handler, Valve's `Vector`, `init.nut` |
| Shipped scripts | **All 92 compile** (64,067 lines); **104 of 106 maps run five seconds of theirs without an error** |
| Tests | 38 in the module (`src/vscript/tests.rs`, `table.rs`, `lexer.rs`, `format.rs`), and 1 depot |

## Quick start

```rust
use crate::vscript::{Value, Vm};

let mut vm = Vm::new(0, Box::new(|text| eprint!("{text}")));
let main = vm.compile(b"function Double(x) { return x * 2 }", "example.nut")
    .expect("compiles");
vm.execute(&mut (), &main, None, &[]).expect("runs");

let root = Value::Table(vm.root());
let double = vm.get(&mut (), &root, &Value::str("Double")).expect("defined");
let four = vm.call(&mut (), &double, root, &[Value::Integer(2)], true);
assert!(matches!(four, Ok(Value::Integer(4))));
```

A native is a closure over `(&mut Vm, &mut dyn Any, &[Value])`, where `args[0]` is
`this` and the `&mut dyn Any` is whatever host the caller passed — the server passes
itself:

```rust
let root = vm.root();
vm.register_native(&root, "Twice", 2, ".n", |_, _, args| {
    Ok(Value::Integer(args[1].to_integer() * 2))
});
```

`2` is Squirrel's parameter count **including `this`**; `".n"` is its type mask (`.`
anything, `n` a number, `s` a string, `x` an instance, …, `|` for alternatives). A
native that fails returns `Err(value)`; `Err(Value::Null)` means "failed, with no message
of my own", which is `return SQ_ERROR` without `sq_throwerror`.

## The core types

### `Vm` (`mod.rs`)

| Item | What it is |
|---|---|
| `Vm::bare()` | `sq_open` — the base library only. For a test of the language. |
| `Vm::new(developer, print)` | `CSquirrelVM::Init`: base library, `sqstdmath`, `sqstdstring`, the error handler, `developer()`, `GetFunctionSignature`, `Vector`, then `init.nut`. |
| `compile(source, name) -> Result<Value, CompileError>` | `sq_compilebuffer`. The value is the file's `main` closure. The error is also printed, as `_sqstd_compiler_error` prints it. |
| `execute(host, function, scope, args)` | `CSquirrelVM::ExecuteFunction`: calls with `this` = `scope` or the root table, reports an uncaught error through the error handler, and starts the "running too long" clock. The entry point for a host. |
| `call(host, function, this, args, raise_error)` | `sq_call` — any callable, from native code, as a new `Execute`. |
| `get(host, object, key)` | `sq_get` — delegates and metamethods, **no** root-table fallback. |
| `new_slot_value(host, object, key, value)` | `sq_newslot` — what Valve's `SetValue` is. |
| `delete_slot_value(host, object, key)` | `sq_deleteslot` — what Valve's `ClearValue` is. |
| `set_slot(table, key, value)` | A raw new slot on a table, for setting up. |
| `register_native` / `native` | A native closure, into a table or as a value. |
| `new_class(base, type_tag)` / `class_new_slot` / `new_instance(class, user)` | Native classes: `sq_newclass`, `class.key <- value`, and `sq_createinstance` + `sq_setinstanceup`. `user` is the instance's `_userpointer`. |
| `new_table` / `new_array` | Containers tracked for teardown (below). |
| `root()` / `consts()` / `developer()` / `print(text)` | |

### `Value` (`value.rs`)

`Null`, `Bool`, `Integer(i32)`, `Float(f32)`, `String(SqStr)`, `Table(TableRef)`,
`Array(ArrayRef)`, `Closure(Rc<Closure>)`, `Native(Rc<Native>)`, `Class(ClassRef)`,
`Instance(InstanceRef)`, `WeakRef(Rc<WeakRef>)`.

- `to_integer()` truncates, and gives `0x80000000` for NaN or out-of-range floats
  (`float_to_int`), which is x86's answer and the shipped game's.
- `is_false()` is null, `false`, 0 and 0.0.
- `real()` reads a weak reference through.
- `raw_equal(a, b)` is `==` between two values of one type: strings by content, floats
  **by bits**, objects by identity.
- `type_name()` is `typeof`'s answer without metamethods. A native closure is
  `"native function"`, which is Squirrel's.

`SqStr` is **bytes**, hashed with `_hashstr`. `to_string_lossy()` is for printing only.

### `vector_value` and `vector_of` (`vector.rs`)

A `Vector` instance from three floats, and the three floats of one. Valve's
`FIELD_VECTOR` in both directions.

## How it behaves

### Calls and `this`

`obj.f(x)` calls `f` with `this` = `obj` — unless `obj` is a class, when it keeps the
caller's. Every other shape of callee (`f(x)` on a local, `(obj.f)(x)`, `vargv[0](x)`)
gets the **caller's** `this`. An unqualified `f(x)` is `this.f(x)`, so it gets the
caller's `this` too.

A closure made by `bindenv(env)` always runs with `this` = `env`, held weakly.

### Name resolution is compile-time

In order: a local of the current function, one of its explicit outers
(`function():(x)`), a constant (`const`/`enum`, from the VM-wide constant table), else
`this.name`. **An enclosing function's locals are not visible** without the outer list.

Reading `this.name` that is not there falls back to the **root table**, but only when
the object is the running function's own `this`.

### Errors and `try`

Errors are values. One reaches the error handler **when it is raised** if the current
`Execute` has no `try` open and was started with `raise_error`. `call` from a native (a
metamethod, `array.sort`'s comparator, `closure.pcall`) starts a new `Execute`. The
default handler prints Valve's `AN ERROR HAS OCCURED [...]`, `CALLSTACK` and `LOCALS`.

### Teardown

`Rc` cannot collect cycles and neither could Valve's VM without its collector, which
Valve turned off. The `Vm` keeps a weak list of every table, array, class and instance
it made and empties them on drop — `sq_close` finalising the chain.

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **Scripts written for Squirrel 3 do not compile.** No `local function`, no
   `base.`, no lambda syntax — it is 2.2.
2. **`local t = {}; f()` on one line is a compile error at file scope** — and fine in a
   block. `SQCompiler::Compile` skips the semicolon check only after a `}`, and a `;`
   right after a `}` is then an empty statement followed by a missing separator. A test
   that writes one-liners has to use newlines.
3. **A newline before `[` is a compile error; before `(` it is a call.**
4. **`1.5.tostring()` does not parse** — parenthesise the literal.
5. **An `Input<name>` function that returns nothing blocks the input.** Return `true`.
6. **`get` does not fall back to the root table**; only a script's own lookup through
   `this` does.
7. **The "running too long" check is Valve's**: every 100,000 back-edges, a script that
   has run more than 30 ms since its `execute` is stopped with "Script terminated by
   SQQuerySuspend".
8. **A native gets its own frame on the call stack**, so a stack trace through one shows
   it as `NATIVE` at line -1, as Squirrel's does.
9. **Script calls nest at most 200 deep** (`MAX_SCRIPT_DEPTH`). Squirrel has no such
   limit; a tree walker recurses on the host stack and would otherwise take the process
   down. No shipped script comes close.

## Deliberate divergences

| Divergence | Why | Where |
|---|---|---|
| A tree walker, not the register bytecode | The bytecode is an encoding; every decision the compiler makes is kept in the AST | `ast.rs`, `interp.rs` |
| `%e`/`%g` print a two-digit exponent | C99 and every POSIX libc; Valve's Windows CRT printed three | `format.rs` |
| `rand()` is the Windows CRT's LCG | Portal 2's primary platform; no shipped script calls it | `stdlib.rs`, `CRand` |
| Script calls nest at most 200 deep | Host stack | `MAX_SCRIPT_DEPTH` |
| A negative string index reads 0 | The C reads past the end of the buffer | `interp.rs`, `fallback_get` |
| A comparator returning a non-number counts as 0 | The C leaves `ret` uninitialised | `baselib.rs`, `sort_compare` |

## What is deliberately absent

`yield`, `resume`, generators, `newthread` and `suspend` — **0 of the 92 shipped
scripts** use any of them. Calling a generator function raises "generators are not
supported". The debugger (`sqdbg`) and VM state serialisation (saves) are absent too.

## Extending it

- **A new global native**: `vm.register_native(&vm.root(), name, nparams, mask, f)`.
  Count `this` in `nparams`, and write the mask Valve's `RegisterFunctionGuts` would
  have (`.` for `this`, then `s`/`n`/`b`/`x`/`.` per parameter).
- **A new native class**: `new_class`, then `class_new_slot` per method, then
  `set_slot(&root, name, Value::Class(class))`. Put the native data on instances with
  `new_instance(&class, Some(Box::new(data)))` and read it back with
  `instance.borrow().user`.
- **A metamethod**: a method named `_get`, `_set`, `_tostring`, … on a class goes into
  its metamethod table rather than its members, exactly as `SQClass::NewSlot` does.

## Which tests guard what

| Test | Guards |
|---|---|
| `integers_are_32_bit_and_wrap`, `floats_are_single_precision_and_print_with_percent_g` | The number model and `%g` |
| `equality_is_by_type_then_value_and_floats_compare_bits`, `comparison_has_one_precedence_level_with_equality`, `truthiness_is_null_false_zero_and_zero_point_zero` | `IsEqual`, `ObjCmp`, `IsFalse` |
| `a_newline_ends_a_statement_and_blocks_an_index`, `compile_errors_are_squirrels` | The statement rules and the compiler's messages |
| `unqualified_names_are_fields_of_this_and_fall_back_to_the_root`, `enclosing_locals_are_invisible_without_an_outer_list`, `a_method_call_binds_this_and_a_grouped_one_does_not` | Name resolution and `this` |
| `default_parameters_and_varargs`, `closures_bindenv_call_and_acall` | `StartCall` and the closure delegate |
| `tables_slots_delegates_and_new_slot`, `table_default_delegate_is_reached_through_a_delegate_chain`, `metamethods_on_a_delegate_and_a_class`, `classes_instances_and_constructors` | The object model |
| `try_catch_throw_and_switch`, `an_uncaught_error_prints_the_callstack_and_a_caught_one_does_not` | Errors and the handler |
| `foreach_over_every_iterable`, `const_and_enum_are_compile_time`, `locals_blocks_and_loops` | Control flow |
| `string_and_array_default_delegates`, `the_standard_library_valve_registers`, `valves_vector_class`, `a_vector_answers_x_y_z_and_nothing_else`, `init_nut_defines_printl_and_the_scope_helpers` | The libraries |
| `table::tests::*`, `lexer::tests::*`, `format::tests::*` | The hash table, the lexer and `printf` |
| `the_quick_start_in_the_rustdoc_runs` | This page's example |
| `every_shipped_script_compiles` (depot) | All 92 of the game's scripts |
