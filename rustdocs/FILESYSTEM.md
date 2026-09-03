# `src/filesystem/` — API reference

The virtual filesystem: search paths, mounts, and reads. Replaces `IFileSystem`'s ~180
pure virtuals with the domain model underneath it — **an ordered list of mounts, each of
which can answer "do you have this relative path, and can I read it as bytes", filtered
by a role tag.**

Porting rationale, C++ inventory and what was deliberately dropped are in
[`portdocs/FILESYSTEM.md`](../portdocs/FILESYSTEM.md). This file is the API.

| | |
|---|---|
| Module | `crate::filesystem` |
| Lines | ~3,600 including tests |
| Tests | 70 (`cargo test filesystem`) |
| Dependencies | `thiserror` only |
| Status | Implemented. Async, `.bsp` pak lumps and `sv_pure` deferred — see [Not implemented](#not-implemented) |

```
src/filesystem/
  mod.rs            Vfs, PathId, ScopedVfs, search order, VPK discovery
  error.rs          VfsError, Result
  path.rs           RelPath, lexically_normalize, make_absolute
  keyvalues.rs      Block/Value/Entry, parse, ConditionalSymbols
  gameinfo.rs       GameInfo, SearchPathOptions, plan_search_paths
  mount/mod.rs      Mount trait, Entry, ReadSeek
  mount/dir.rs      DirMount
  mount/vpk.rs      VpkMount
```

---

## Quick start

```rust
use crate::filesystem::{self, PathId, SearchPathOptions, Vfs, VfsError};

// Note: `VfsError` has no `From<io::Error>` — an io error has to carry the path
// it happened on, which `?` can't supply. Wrap explicitly, as the launcher does.
let base_dir = std::env::current_dir().map_err(|e| VfsError::io(".", e))?;
let game_dir = filesystem::locate_game_dir(&base_dir, "portal2");  // the -game value

let vfs = Vfs::mount_game(&game_dir, &base_dir, &SearchPathOptions::default())?;

for w in vfs.warnings() {
    eprintln!("filesystem: {w}");
}

let cfg: Vec<u8> = vfs.read("cfg/config.cfg")?;
let exists       = vfs.exists("materials/metal/wall.vmt");
let mut stream   = vfs.open("maps/sp_a1_intro1.bsp")?;   // Read + Seek
let maps         = vfs.list("maps")?;

// Restrict to one role (see Scoping below).
let so = vfs.scoped(PathId::GameBin).read("libtier0.so")?;
```

Errors are `VfsError`; `filesystem::Result<T>` is the alias.

---

## Core types

### `Vfs`

The whole filesystem. **Not `Clone`.** The engine owns one and passes `&Vfs` down; there
is no `g_pFullFileSystem` equivalent and one should not be added.

`Vfs: Send + Sync` (asserted by `public_types_are_thread_safe`), so `&Vfs` can be shared
across threads. All read methods take `&self`.

**Construction**

```rust
Vfs::mount_game(game_dir: &Path, base_dir: &Path, options: &SearchPathOptions) -> Result<Vfs>
```

Reads `<game_dir>/gameinfo.txt`, builds the search path list, and mounts every directory
and VPK it names. Fully initialized on return — there is no `Connect`/`Init`/`Shutdown`
lifecycle and no reachable half-constructed state.

Fails only if `gameinfo.txt` is missing (`GameInfoMissing`) or structurally wrong
(`GameInfoInvalid`). **A search path that does not exist on disk is not an error** —
`gameinfo.txt` routinely lists optional DLC and localization directories. Non-fatal
problems land in `warnings()`.

**Reading**

```rust
fn read  (&self, path: &str) -> Result<Vec<u8>>
fn open  (&self, path: &str) -> Result<Box<dyn ReadSeek>>
fn exists(&self, path: &str) -> bool
fn list  (&self, dir:  &str) -> Result<Vec<Entry>>
```

Paths are relative, `/` or `\` separated, any case. Normalized internally; see
[`RelPath`](#relpath).

`list` takes `""`, `"/"` or `"."` for the mount root. Results merge across mounts,
deduplicated case-insensitively, earlier mounts winning — the same precedence as reads.

**Scoping**

```rust
fn scoped(&self, path_id: PathId) -> ScopedVfs<'_>
```

**Mounting and inspection**

```rust
fn add_search_path(&mut self, path_id: PathId, dir: &Path)   // dir + its pakNN VPKs
fn push_mount(&mut self, path_id: PathId, mount: Arc<dyn Mount>)
fn warnings(&self) -> &[String]
fn search_paths(&self) -> impl Iterator<Item = (PathId, String)> + '_
fn write_root(&self) -> &Path
fn write_path(&self, path: &str) -> Result<PathBuf>
```

`search_paths()` is `CBaseFileSystem::PrintSearchPaths`. Comparing its output against a
stock build is the cheapest end-to-end check of the port and **has not been done yet** —
it needs a real Portal 2 install.

### `PathId`

The role tag on each search path. Replaces Valve's interned path-ID strings, so there is
no `g_PathIDTable`, no `CUtlSymbol`, and no `//pathid/file` string syntax to parse.

| Variant | Is | Searched by an unscoped lookup? |
|---|---|---|
| `Game` | all readable game content | **yes** |
| `Platform` | `<basedir>/platform` | **yes** |
| `Mod` | the active mod dir (first `game` path only) | no — by request only |
| `GameBin` | `<mod>/bin` | no — by request only |
| `ExecutablePath` | directory holding the executable | no — by request only |

`PathId::is_by_request_only(self) -> bool` gives the third column. It reproduces
`MarkPathIDByRequestOnly`, which is a **correctness** property, not just a speed one: a
bare lookup for `bin/foo` must not resolve to `<mod>/bin/foo`.

Absent on purpose: `CONTENT` (authoring-only, and broken on POSIX in the original),
`DEFAULT_WRITE_PATH` (there is one write root — `write_root()` — not a search path), and
`BSP` (never a path ID; it was a lookup-time filter, and becomes a real mount at map
load).

### `ScopedVfs<'a>`

A `Copy` view restricted to one `PathId`. Same four read methods as `Vfs`.

```rust
vfs.scoped(PathId::Mod).read("gameinfo.txt")?;      // only the mod directory
```

### `VfsError` and `Result<T>`

```rust
pub type Result<T> = std::result::Result<T, VfsError>;

pub enum VfsError {
    NotFound        { path: String },
    InvalidPath     { path: String, reason: &'static str },
    Io              { path: String, source: std::io::Error },
    GameInfoMissing { dir: PathBuf },
    GameInfoInvalid { path: PathBuf, reason: String },
    Vpk             { path: PathBuf, reason: String },
    KeyValues       { source_name: String, line: usize, reason: String },
}
```

`NotFound` carries the *normalized* path, not the caller's spelling.

### `RelPath`

A normalized relative path. Construct with `RelPath::new(&str) -> Result<RelPath>`.
`Vfs`'s methods take `&str` and normalize internally, so you only need this when
implementing a `Mount`.

Normalization collapses `/` and `\` runs, drops `.`, resolves `..` lexically, and
**rejects** a `..` that escapes the root (`InvalidPath`) rather than clamping it.

It carries **two spellings of the same path**:

```rust
p.as_str()   // "Materials/Metal/Wall.VMT"  — caller's case preserved
p.folded()   // "materials/metal/wall.vmt"  — ASCII-lowercased lookup key
```

Both exist because `DirMount` tries the caller's exact casing first (one `open`, no
directory enumeration) and only falls back to the folded index on a miss, while
`VpkMount` always keys on `folded()`. ASCII rather than Unicode lowercasing matches
Valve's `V_strlower`, so a name written by Valve's tooling folds to identical bytes.

Also: `components()`, `folded_components()`, `folded_split() -> (Option<&str>, &str)`.

Free functions in `path`: `lexically_normalize(&Path) -> PathBuf` (resolves `.`/`..`
without touching disk or following symlinks) and `make_absolute(base, location)`.

### `Entry` and `ReadSeek`

```rust
pub struct Entry { pub name: String, pub is_dir: bool }   // a name, not a path
pub trait ReadSeek: Read + Seek + Send {}                 // blanket impl
```

`Box<dyn ReadSeek>` replaces `FileHandle_t` (`typedef void *`). There is no `Close` — it
closes on drop. Streams are `Send` but not `Sync`.

---

## Search order

`mount_game` produces the list below for a Portal 2-shaped `gameinfo.txt` whose
`SearchPaths` are `|gameinfo_path|.`, `portal2_dlc1`, `portal2`:

```
Mod             <base>/portal2
Mod             <base>/portal2/pak01_dir.vpk
GameBin         <base>/portal2/bin
Game            <base>/portal2
Game            <base>/portal2/pak01_dir.vpk
GameBin         <base>/portal2_dlc1/bin
Game            <base>/portal2_dlc1
Game            <base>/platform
ExecutablePath  <exe dir>
```

Rules, from `FileSystem_AddLoadedSearchPath`:

1. Location prefixes: `|gameinfo_path|` is relative to the `gameinfo.txt` directory,
   `|all_source_engine_paths|` and bare locations to the base directory. Prefix matching
   is case-insensitive.
2. A `game`-tagged entry expands *before* the entry itself is added: optional
   language/`_lv`/`_tempcontent` siblings, then `Mod` for the **first** `game` entry
   only, then `GameBin` for `<path>/bin`.
3. `<basedir>/platform` is appended as `Game`.
4. `executable_dir`, if supplied, is appended as `ExecutablePath`.
5. Duplicate `(path_id, dir)` pairs collapse — the third entry above resolves to the same
   directory as the first, so its `GameBin`/`Game` pair is dropped.
6. Each directory is scanned for `pak01_dir.vpk` … `pak98_dir.vpk`, stopping at the first
   gap.

The mod directory appears twice (as `Mod` and as `Game`), so its VPKs are discovered
twice — but they are **parsed once and shared behind an `Arc`**. This matters: a shipping
`pak01_dir.vpk` carries tens of MB of directory blob plus a six-figure entry map.

---

## Writing

There is one write root, reachable directly rather than through the search list —
`DEFAULT_WRITE_PATH` was a search path in the original only because everything was, and
writes never actually searched.

```rust
vfs.write_root();                        // -> &Path, the mod directory
vfs.write_path("cfg/config.cfg")?;       // -> PathBuf, normalized and joined
```

Nothing here performs writes; callers use `std::fs` against the resolved path.

---

## Submodule APIs

### `filesystem::keyvalues`

Reader for Valve's KeyValues text format. Text only — the binary pooled format is a
console optimization and is deleted.

```rust
pub fn parse(source_name: &str, text: &str) -> Result<Block>

pub enum  Value { String(String), Block(Block) }
pub struct Entry { pub key: String, pub value: Value }
pub struct Block { /* ordered Vec<Entry> */ }

impl Block {
    fn entries(&self) -> &[Entry];
    fn is_empty(&self) -> bool;
    fn find(&self, key: &str) -> Option<&Value>;        // case-insensitive, first match
    fn find_block(&self, key: &str) -> Option<&Block>;
    fn find_string(&self, key: &str) -> Option<&str>;
    fn values(&self) -> impl Iterator<Item = (&str, &str)>;  // leaf pairs, skips blocks
    fn first_block(&self) -> Option<&Block>;
}

pub struct ConditionalSymbols;
impl ConditionalSymbols { pub fn get(name: &str) -> bool; }
```

`values()` is `GetFirstValue`/`GetNextValue` — **it skips nested blocks**, which is what
`FileSystem_LoadSearchPaths` iterates. `first_block()` finds the outer `"GameInfo"`
wrapper positionally, because Valve locates it that way and mods do rename it.

### `filesystem::gameinfo`

```rust
pub const GAMEINFO_FILENAME: &str = "gameinfo.txt";

pub struct GameInfo {
    pub title: Option<String>,           // the `game` key
    pub steam_app_id: Option<u32>,
    pub search_paths: Vec<SearchPathSpec>,   // verbatim, in order
}
impl GameInfo {
    fn load(dir: &Path) -> Result<Self>;
    fn parse(path: &Path, text: &str) -> Result<Self>;
}

pub struct SearchPathSpec { pub path_id: String, pub location: String }

#[derive(Default)]
pub struct SearchPathOptions {
    pub language: Option<String>,       // adds `<path>_<lang>` siblings
    pub low_violence: bool,             // `-lv`; false on POSIX otherwise
    pub temp_content: bool,             // `-tempcontent`
    pub executable_dir: Option<PathBuf>,
}

pub struct PlannedPath   { pub path_id: PathId, pub dir: PathBuf }
pub struct SearchPathPlan {
    pub paths: Vec<PlannedPath>,
    pub mod_dir: Option<PathBuf>,
    pub warnings: Vec<String>,
}

pub fn plan_search_paths(
    info: &GameInfo, gameinfo_dir: &Path, base_dir: &Path, options: &SearchPathOptions,
) -> SearchPathPlan;
```

`plan_search_paths` is pure — no filesystem access except the localization-directory
probe — which is why the ordering rules are unit-testable without an install.

Only `FileSystem.SearchPaths`, `FileSystem.SteamAppId` and `game` are read. `GameData`,
`ToolsAppId`, `singleplayer_only` and the `Game_LowViolence` variants are left unparsed
until something needs them.

### `filesystem::mount`

```rust
pub trait Mount: Send + Sync {
    fn open(&self, path: &RelPath) -> Option<Result<Box<dyn ReadSeek>>>;
    fn read(&self, path: &RelPath) -> Option<Result<Vec<u8>>>;
    fn contains(&self, path: &RelPath) -> bool;
    fn list(&self, dir: Option<&RelPath>, out: &mut Vec<Entry>);   // None = mount root
    fn describe(&self) -> String;
}
```

**`Option<Result<T>>` is load-bearing.** `None` means "not mine, keep searching";
`Some(Err(_))` means "mine, and reading it failed". Collapsing them is how the original
ends up treating an unreadable file as a missing one and silently falling through to a
stale copy in a later search path.

`read` is separate from `open` because a VPK can serve a whole file without building a
seekable reader, and whole-file reads dominate.

**`DirMount`** — `DirMount::new(root: impl Into<PathBuf>)`, `root() -> &Path`. Does not
touch the filesystem at construction. Exact-match fast path, then a per-directory
case-folded index built lazily and cached behind an `RwLock`. Every component is folded,
not just the filename, because Valve content miscases directories too.

**`VpkMount`** — `VpkMount::open(path: impl AsRef<Path>) -> Result<Self>`, `len()`,
`is_empty()`. Accepts `foo_dir.vpk` or `foo.vpk` and derives sibling `foo_NNN.vpk` paths.
Handles v1, v2 and headerless directories, files spanning numbered archives, and data
embedded in the dir file (`archive == 0x7fff`).

---

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **Unscoped lookups skip `Mod`, `GameBin` and `ExecutablePath`.** `vfs.read("libtier0.so")`
   will *not* find `<mod>/bin/libtier0.so`; use `vfs.scoped(PathId::GameBin)`. This is
   intentional and matches `MarkPathIDByRequestOnly`. (The file is still reachable
   unscoped as `bin/libtier0.so`, because the mod directory is itself a `Game` path.)
2. **`..` that escapes the root is an error, not a clamp.** `read("../secret")` returns
   `InvalidPath`. Do not "fix" this by clamping.
3. **`$WIN32` does not mean Windows.** Valve's `DefaultConditionalSymbolProc` resolves it
   to `IsPC()` — "not a game console" — so `[$WIN32]` in `gameinfo.txt` is **true on
   Linux and macOS**. `$WINDOWS` is the one that means Windows, and it is false. Getting
   this backwards silently drops search paths, and the failure surfaces much later as
   missing content. See `ConditionalSymbols`.
4. **KeyValues escape sequences are off.** Valve only enables them via
   `UsesEscapeSequences(true)`, which nothing calls for `gameinfo.txt`. A `\` is a
   literal character; Valve content is full of Windows-authored paths that depend on it.
5. **VPKs are ordered mounts, placed *after* the directory they were found in.** This is
   a deliberate divergence: the original keeps VPKs in a separate global list consulted
   before every search path, so VPK content wins over loose files unconditionally. Here a
   loose file overrides the VPK beside it. **Unverified against a real install.**
   `VPKS_AFTER_LOOSE_FILES` in `mod.rs` is the single switch to revert it.
6. **`Block::values()` skips nested blocks**, and `Block::find*` is case-insensitive and
   returns the *first* match while duplicates are preserved in order. `SearchPaths`
   depends on both halves of that.
7. **VPK entries are stored fully lowercased** by Valve's writer — directory, basename
   *and* extension — with the directory's trailing slash stripped, and `" "` (a single
   space) standing in for an empty directory or absent extension.
8. **A VPK file's contents are its metadata blob followed by its archive parts.** The
   metadata is a preload prefix, not a sidecar; a file's logical length is
   `metadata_size + part lengths`.
9. **Missing directories are normal**, not errors. Only `gameinfo.txt` itself is
   required.
10. **`Vfs` is not `Clone`.** Share `&Vfs`. Don't reintroduce a global.

---

## Not implemented

Deferred deliberately, with the reasoning in `portdocs/FILESYSTEM.md`:

- **Async I/O.** `basefilesystemasync.cpp`'s callback API with manual buffer ownership
  should not survive contact with Rust, and nothing on the boot path needs it. Picking a
  concurrency model before there is a consumer to measure is the wrong order.
- **`.bsp` embedded pak lump mounts.** Needed at map load, not before. `Mount` +
  `push_mount` is the seam; a `BspPakMount` implementing the trait is the whole job.
- **`sv_pure` file tracking.** Dropped for single-player Portal 2. If multiplayer returns:
  the original computes hashes *during* reads (`CPackedStore::RegisterFileTracker`), so
  the tee point is inside `VpkMount`'s read path, not after it.
- **`QueuedLoader`.** A map-load prefetch optimization with no correctness role.
- **VPK writing.** Tooling, not runtime.
- **Plain-zip mounts.** Whether anything still needs them at runtime is an open question.

`#![allow(dead_code)]` sits at the top of `mod.rs` because the read API has no consumer
until `src/engine/` exists. **Remove it when it does.**

---

## Extending it

To add a mount type: implement `Mount`, then `vfs.push_mount(path_id, Arc::new(m))`.
Respect the `Option<Result<_>>` contract in gotcha #4 above, key lookups on
`RelPath::folded()` unless you have a reason to preserve case, and make the type
`Send + Sync` — `public_types_are_thread_safe` will catch you if not.

To change search path construction, edit `gameinfo::plan_search_paths`; it is pure and
its ordering is covered by unit tests that read like a specification.

---

## Test coverage

79 tests, all in-module. Notable ones to look at before changing behavior:

| Test | Guards |
|---|---|
| `full_ordering_for_a_portal2_shaped_gameinfo` | the exact search path order and dedup |
| `by_request_only_roles_are_skipped_unscoped` | gotcha #1 |
| `win32_conditional_is_kept_on_posix` | gotcha #3 |
| `backslashes_are_literal_not_escapes` | gotcha #4 |
| `metadata_is_prepended_to_the_contents` | gotcha #8 |
| `reads_a_vpk_from_an_independent_encoder` | the VPK reader against a fixture built by a **separately written** encoder, so the parser is checked against the spec rather than against this module's own test builder |
| `resolves_wrong_case_in_every_component` | case folding across all path components |
| `traversal_out_of_the_mount_is_rejected` | gotcha #2 |
| `public_types_are_thread_safe` | the `Send + Sync` guarantees above |

The one verification still outstanding needs a real Portal 2 install: comparing
`vfs.search_paths()` against a stock build's `PrintSearchPaths()`, and reading a real
`pak01_dir.vpk`.
