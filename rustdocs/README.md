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

## Current docs

- [`FILESYSTEM.md`](FILESYSTEM.md) — `src/filesystem/`. `Vfs`, `PathId`, mounts,
  `gameinfo.txt` parsing, the KeyValues reader, and VPK reading.

## Why these exist

Porting sessions lose context. A cold-started session that has to re-derive an API by
reading 3,500 lines of source will burn most of its budget doing so, and is likely to
miss the non-obvious rules — which lookups skip which mounts, why a path type carries two
spellings of the same string, which behaviors deliberately diverge from Valve's. Those
belong in prose, once, next to the code they describe.

Rustdoc comments in the source stay the authority on individual items; these files carry
the parts that don't fit on a single item — cross-cutting semantics, worked examples, and
the "why is it like this" that a `///` on one function can't hold.
