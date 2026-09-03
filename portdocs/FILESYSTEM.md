# Porting `filesystem`

> Written against the current `PORTING.md` architecture (one crate, one binary, no FFI,
> `legacy/` decoupled). No superseded-plan banner needed — unlike `LAUNCHER.md` and
> `ENGINE.md`, this doc has never assumed the old model.

`filesystem` is the next module on the boot path. `PORTING.md`'s "Porting order" calls
for "filesystem (enough to read `gameinfo.txt` and mount VPKs)" immediately after
bootstrap, because nothing else — not windowing, not rendering, not the host loop — can
be exercised against real content until the game's files can be found and read.

All paths below are relative to the original tree; prefix with `legacy/` to open them.

---

## Scale, stated plainly

The module is smaller than it looks, because a large fraction of it is either
console-only, dead, or slated for replacement.

### `filesystem/` — 28,415 lines in the directory

Only some of it is in the build. From `filesystem/CMakeLists.txt`:

| File | Lines | In build? | Disposition |
|---|---:|---|---|
| `basefilesystem.cpp` | 9,475 | yes | The module. Search paths, open path, pack files, VPK glue, find-first/next |
| `basefilesystem.h` | 1,398 | — | All the internal types (`CSearchPath`, `CPackFile`, `CFileHandle`, …) |
| `filesystemasync.cpp` | 2,546 | yes | **Dead** — the "new async filesystem". See below |
| `QueuedLoader.cpp` | 2,145 | yes | Map-load bulk prefetch. Optimization, not required |
| `filesystem_stdio.cpp` | 2,125 | yes | The concrete POSIX/stdio backend + the low-level `FS_*` wrappers |
| `basefilesystemasync.cpp` | 1,480 | yes | The async that is *actually* used (`IFileSystem::AsyncRead*`) |
| `filetracker.cpp` | 1,262 | yes | `sv_pure` CRC/MD5 whitelist tracking |
| `linux_support.cpp` | 261 | POSIX only | `FindFirstFile`/`FindNextFile` emulation + case-insensitive lookup |
| `filegroup.cpp` | 2,060 | **no** | PS3 |
| `XboxInstaller.cpp` | 1,702 | **no** | X360 |
| `filesystem_steam.cpp` | 1,502 | **no** | Steam-cache backend, not built |
| `filesystem_async.cpp` | 1,348 | **no** | Older async, superseded |

Pulled in from elsewhere by the same target: `public/zip_utils.cpp` (1,754) and
`public/kevvaluescompiler.cpp` (435).

### Outside `filesystem/`

| Path | Lines | What it is |
|---|---:|---|
| `public/filesystem.h` | 1,027 | `IBaseFileSystem` + `IFileSystem` — the interface everything else sees |
| `public/filesystem_init.cpp` | 1,611 | **`gameinfo.txt` → search paths.** The bootstrap. Not part of the filesystem target; compiled into each consumer |
| `public/filesystem_init.h` | 223 | Its interface |
| `vpklib/packedstore.cpp` | 2,115 | VPK reader/writer |
| `public/vpklib/packedstore.h` | 463 | VPK types |
| `public/filesystem/iasyncfilesystem.h` | 559 | **Dead** — see below |
| `public/filesystem/IQueuedLoader.h` | 164 | QueuedLoader interface |
| `public/filesystem/IXboxInstaller.h` | 89 | X360, drop |

### What's actually in scope

Dropping console files, the dead async stack, `sv_pure` tracking, and the QueuedLoader
optimization leaves roughly **13–15k lines of C++ to read** — `basefilesystem.cpp` +
`filesystem_stdio.cpp` + `basefilesystemasync.cpp` + `filesystem_init.cpp` +
`packedstore.cpp` — and of those, a substantial fraction is `_X360`/`_PS3`/`WIN32`
branching to skim past (130 such references in `basefilesystem.cpp` alone, 41 in
`filesystem_stdio.cpp`).

### Two dead subsystems, confirmed

**`IAsyncFileSystem` / `CAsyncFileSystem` is built but unused.** `filesystemasync.h`
(596) + `filesystemasync.cpp` (2,546) + `iasyncfilesystem.h` (559) — about 3,700 lines —
implement a second, newer async filesystem with request objects, groups, and search
requests. It's instantiated (`filesystem_stdio.cpp:580`) and registered
(`interfaces.cpp:121`, `"VNewAsyncFileSystem001"`), but a tree-wide search for
`IAsyncFileSystem`/`g_pAsyncFileSystem` returns only its own implementation files, the
interface-registration table, and a forward declaration in
`public/resourcesystem/iresourcesystem.h`. **Nothing calls it.** Don't port it; don't
read it for design ideas either — the async the engine actually uses is the older
`IFileSystem::AsyncRead*` family in `basefilesystemasync.cpp`.

**`filesystem_steam.cpp` (1,502) is not in the build.** `IsSteam()` is effectively
always false in this tree, which means every `if ( IsSteam() )` branch in
`filesystem_init.cpp` — `MountSteamContent`, the Steam env-var setup, `SetSteamAppUser`,
`GetSteamExtraAppId` — is dead on arrival. That deletes a meaningful chunk of the
bootstrap file.

---

## What the module does

Three layers, worth separating in your head because the port treats them differently:

1. **Path resolution** (`basefilesystem.cpp`). A relative game path like
   `materials/metal/wall.vmt` is resolved against an ordered list of search paths, each
   tagged with a *path ID*. This is the actual value of the module and the part to port
   faithfully.
2. **Container readers.** A resolved file may live on disk, inside a VPK
   (`vpklib/packedstore.cpp`), inside a `.bsp`'s embedded pak lump, or inside a plain
   zip (`public/zip_utils.cpp`). Each presents a seekable byte stream.
3. **Byte-level I/O** (`filesystem_stdio.cpp`). `FS_fopen`/`FS_fread`/`FS_stat`
   wrappers, buffering, unbuffered/aligned reads, and the POSIX case-insensitivity
   fallback. Almost entirely replaced by `std::fs` + `std::io`.

Sitting on top, and technically not part of the module: **`public/filesystem_init.cpp`**,
which reads `gameinfo.txt` and calls `AddSearchPath` in the right order. It's compiled
into every consumer rather than into the filesystem library. For the Rust port this
distinction is meaningless — it becomes part of the same module.

---

## The interface surface, and why it's discarded

`IFileSystem` (`public/filesystem.h:549`) inherits `IAppSystem` and `IBaseFileSystem`
and declares roughly 180 pure-virtual methods. It is exactly the kind of interface
`PORTING.md`'s polarity rule exists to prevent transliterating. A representative sample
of what's in there:

- Real file I/O: `Open`/`Read`/`Write`/`Seek`/`Tell`/`Size`/`Close`, `ReadFile`,
  `ReadFileEx`, `ReadToBuffer`.
- Search path management: `AddSearchPath`, `RemoveSearchPath`, `MarkPathIDByRequestOnly`,
  `RelativePathToFullPath`, `GetSearchPath`.
- Iteration: `FindFirst`/`FindNext`/`FindClose`/`FindFirstEx`/`FindFileAbsoluteList`.
- **`LoadModule`/`UnloadModule`** — `dlopen` through the filesystem. Deleted outright;
  there is no module loading in a single static binary.
- 20+ `Async*` methods with handle-based lifetime (`AsyncAddRef`/`AsyncRelease`).
- 12 whitelist/CRC methods for `sv_pure`.
- ~15 X360 DLC methods (`DiscoverDLC`, `IsAnyCorruptDLC`, `AddDLCSearchPaths`, …).
- PS3 HDD-cache prefetch methods, `_PS3`-gated.
- `KeyValuesPreloadType_t` + `LoadKeyValues` — a compiled-KeyValues fast path.
- Xbox install state: `IsLaunchedFromXboxHDD`, `IsInstalledToXboxHDDCache`, …

Handles are `typedef void *FileHandle_t`, errors are `bool` returns or sentinel values,
and lifetime is documented rather than enforced. **Design the Rust API from the domain
model in the next section, not from this list.**

---

## Internal architecture

### Search paths

The core state (`basefilesystem.h:754`) is `CUtlVector<CSearchPath> m_SearchPaths` — an
ordered list, searched front to back. Each `CSearchPath` holds:

- a filesystem path (`CUtlSymbol`, interned),
- a `CPathIDInfo*` — the **path ID**, also interned, shared between search paths with the
  same ID,
- an optional `CPackFile*` — if set, this search path *is* a pack file rather than a
  directory,
- `m_storeId`, used to deduplicate visits,
- `m_bIsLocalizedPath`, `m_bIsDvdDevPath`.

**Path IDs** are the module's central concept: a string tag grouping search paths by
role. A lookup either specifies a path ID (search only paths with that tag) or passes
`NULL` (search everything not marked by-request-only). The canonical set, all established
in `filesystem_init.cpp`:

| Path ID | Meaning | Established at |
|---|---|---|
| `GAME` | Everything readable as game content | `filesystem_init.cpp:701` |
| `MOD` | The *first* `game`-tagged path only — the active mod dir | `:690` |
| `GAMEBIN` | `<mod>/bin` | `:311` (`AddGameBinDir`) |
| `PLATFORM` | `<basedir>/platform` | `:820`, `:1591` |
| `EXECUTABLE_PATH` | Directory of the executable | `:1406` |
| `DEFAULT_WRITE_PATH` | Where writes land — the mod dir | `:877` |
| `CONTENT` | Authoring-content mirror, tools only | `:860` |
| `BSP` | Not a real path ID — a lookup-time filter (see below) | `basefilesystem.h` `FilterByPathID` |

`MarkPathIDByRequestOnly` (`filesystem_init.cpp:865-872`) marks `content`,
`executable_path`, `gamebin`, and `mod` as by-request-only, so a `NULL`-path-ID lookup
skips them. This is a load-bearing optimization *and* a correctness property: a bare
lookup for `bin/foo` must not find `<mod>/bin/foo`.

**`BSP` is a hack, not a path ID** (`basefilesystem.h`, `FilterByPathID`): asking for
path ID `BSP` searches `GAME` paths but accepts *only* those whose pack file has
`m_bIsMapPath` set — i.e. the currently-loaded map's embedded pak lump. Worth
representing explicitly in Rust rather than reproducing as a magic string.

**The `//pathid/file` syntax** (`ParsePathID`, `basefilesystem.cpp:4309`): a filename
beginning with `//` has its path ID parsed out of the leading component, so
`//MOD/cfg/config.cfg` means "`cfg/config.cfg`, path ID `MOD`". `//*/` means "no path
ID". Note the comment Valve left there — `FIXME: Pain!` — because `V_FixSlashes` is
called all over the codebase and will happily mangle this. A Rust API that takes a
`(PathId, &str)` pair instead of encoding the path ID into the string makes the whole
problem disappear.

### The open path

`OpenForRead` → `FindFileInSearchPaths` (`:4204`) → per-path `FindFile` (`:4131`).
For each search path in order:

1. If the filename is absolute, check whether it's of the form
   `/a/b/c.zip/materials/x.vtf` and open from inside the zip (`HandleOpenFromZipFile`).
2. **Otherwise, loop over every mounted VPK** and return the first hit (`:4166-4185`).
3. Otherwise, if this search path *is* a pack file, open from inside it.
4. Otherwise, concatenate search path + filename and open from disk
   (`HandleOpenRegularFile`).

**Step 2 is the quirk worth understanding.** VPKs are not entries in `m_SearchPaths`;
they live in a separate global list, `m_VPKFiles` (`basefilesystem.h:1153`). The whole
list is re-scanned on *every* search path iteration. Two consequences:

- **VPK content unconditionally wins over loose files**, regardless of where the VPK was
  mounted relative to the directory in the search order. Loose-file overrides of VPK
  content do not work the way the search path list implies they should.
- The scan is O(paths × VPKs) with the same answer every iteration.

I'd call this a bug rather than a design, but it has been shipped behavior for a decade
and mods rely on it either way. **Decide explicitly** whether the Rust port keeps
VPK-wins-always or makes VPKs ordinary ordered mounts (my recommendation: ordered mounts,
which is both more predictable and what people expect — but it must be a stated decision,
and it should be verified against a real Portal 2 install before committing).

### VPK format (`vpklib/`)

Fixed format — Valve's depots produce it, so per `PORTING.md` the byte layout is
immovable even though the parsing mechanism should be modernized (`binrw`/`deku`).

- A `_dir.vpk` file holds the directory; bulk data lives in numbered siblings
  `_000.vpk`, `_001.vpk`, … A chunk referring to file number `0x7fff`
  (`VPKFILENUMBER_EMBEDDED_IN_DIR_FILE`) is embedded in the dir file itself.
- Header (`vpklib/packedstore_internal.h`): magic `0x55aa1234`, version, directory size.
  **Version 2 adds** embedded-chunk size, chunk-hashes size, self-hashes size, and
  signature size; version 1 has only the first three fields (`VPKDirHeaderOld_t`), and
  `packedstore.cpp:313-333` handles both plus a headerless raw-directory case.
- The directory is a three-level nested string structure — extension → directory →
  basename, each level NUL-terminated with an empty string as terminator — with each file
  entry carrying CRC32, a metadata blob, and a list of `(file number, offset, size)` part
  descriptors terminated by a sentinel. Documented informally in `vpklib/fileformat.txt`
  and structurally in `packedstore.cpp:44` (`CFileHeaderFixedData`).
- `BuildHashTables` (`packedstore.cpp:225`) builds extension/directory hash chains for
  lookup. In Rust this is just a `HashMap` built once at mount.
- `MAX_ARCHIVE_FILES_TO_KEEP_OPEN_AT_ONCE` is 512 — the reader keeps data-file handles
  cached rather than reopening per read.
- MD5 chunk hashes and the optional signature block exist for `sv_pure` verification.
  Parse past them; don't implement verification unless multiplayer comes into scope.

**Writing VPKs is out of scope.** `packedstore.cpp` contains a full writer
(`EPADD_NEWFILE`, chunk rebuilding, signing) used by `vpk.exe`-style tooling. The game
only reads.

### Automatic VPK discovery, and a CS:GO-ism

`AddSearchPath` (`basefilesystem.cpp:2842`) does considerably more than add a path:

- **It scans for `pak01_dir.vpk` … `pak98_dir.vpk`** in the directory being added,
  stopping at the first gap, and mounts each (`:2952-2971`). Note it probes
  `pakNN_dir.vpk` with a raw `fopen` but then mounts `pakNN.vpk` — the `CPackedStore`
  constructor re-derives the `_dir` suffix.
- **`const char *pGameName = "csgo";` is hardcoded** (`:2851`). If the path being added
  contains `"csgo"` and the path ID is `GAME`/`MOD`/`PLATFORM`, it additionally mounts a
  sibling `update/` directory and scans for `csgo_dlc1`, `csgo_dlc2`, … up to 99,
  respecting a `dlc_disabled.txt` marker, mounting them in reverse order so higher DLC
  numbers take priority.
- **`COMPAT:` path-ID prefix** (`:2854`) maps to `csgo/pakxv_<name>.vpk` when running
  with VPKs, or `csgo/compatibility/<name>/` otherwise.

This is the concrete CS:GO-shaped default `PORTING.md` warns about, and it's the one that
actually matters in this module. For Portal 2 the update/DLC layout needs to come from
config rather than a hardcoded game name — **and the real directory names need checking
against an actual Portal 2 install**, since I can't verify them from this tree (no assets
here, and no `gameinfo.txt` anywhere in the repo).

### Case sensitivity — the sharpest portability edge

Valve content references files with inconsistent casing. On Windows and on
case-insensitive macOS volumes this is invisible. On Linux it isn't, so
`filesystem/linux_support.cpp:208` provides `findFileInDirCaseInsensitive`, called as a
**fallback after a case-sensitive miss** from five sites: `FS_stat`
(`filesystem_stdio.cpp:1029`), `FS_chmod` (`:977`), one more in `filesystem_stdio.cpp`
(`:1173`), and two in `basefilesystem.cpp` (`:4788`, `:4879`).

The implementation is poor in three specific ways, all worth fixing rather than porting:

1. It `scandir()`s the entire containing directory on every miss — no caching. Content
   with systematically wrong casing turns every open into a directory enumeration.
2. The scandir filter predicate compares against a **file-scope `static char
   fileName[MAX_PATH]`** (`linux_support.cpp:196`), so it is not thread-safe. The
   filesystem is otherwise explicitly threaded (`m_SearchPathsMutex`, per-pack-file
   mutexes, the async thread pool).
3. It's `#if defined(LINUX)` only. **macOS is a first-class target for us** and APFS can
   be case-sensitive; such a volume would break content loading in ways Valve never had
   to care about.

The Rust port should build a **case-folded index per mount**, not retry-on-miss. VPK
mounts get this for free — the directory is already fully enumerated at mount time, so
key the map on the folded name. Directory mounts can index a directory lazily on first
access and cache it. This makes the behavior identical on Linux and macOS and removes the
per-miss `scandir` entirely.

### Async I/O

The live implementation is `basefilesystemasync.cpp`: a thread pool (`CreateNewThreadPool`,
`:712`, named `"FsAsyncIO"`) executing `CFileAsyncJob`s, with priorities, abort,
suspend/resume, refcounted handles, and an `AsyncFinishAll(priority)` drain. Requests are
`FileAsyncRequest_t` (`public/filesystem.h:376`): filename, optional caller buffer,
offset, byte count, completion callback, opaque context, priority, flags, path ID,
optional custom allocator. Flags cover "allocate but don't free", "free after callback",
"actually do it synchronously", and "null-terminate the buffer".

Consumers are real — map loading, sound streaming, model/texture loading. But the
*shape* is a C callback API with manual buffer ownership, and it should not survive
contact with Rust. Options, in the order I'd consider them:

- Make the core `Vfs` synchronous and give callers `std::thread`/`rayon` for
  parallelism. Simplest; probably enough to boot.
- A small job queue returning a handle you can await or poll, with the buffer owned by
  the returned value rather than by flags.
- A full async runtime (`tokio`). Almost certainly overkill for what is memory-bandwidth-
  and decompression-bound rather than concurrency-bound.

**Recommendation: build the synchronous core first and defer this decision.** Nothing on
the boot path needs async, and picking a concurrency model before there's a consumer to
measure is the wrong order. Note this explicitly as deferred rather than done.

`QueuedLoader.cpp` sits on top of async and exists to make map loads fast by batching all
I/O up front. It's an optimization with real fan-in (engine, materialsystem, datacache)
but no correctness role. Skip it initially.

### `sv_pure` / whitelist tracking

`filetracker.cpp` (1,262) plus the `RegisterFileWhitelist`/`CacheFileCRCs`/
`GetUnverifiedFileHashes` interface methods and the VPK MD5 chunk-hash machinery exist so
a server can verify a client's files are unmodified. For a single-player-focused Portal 2
target this is **droppable in its entirety** — but it's the one piece of the module whose
removal is hard to reverse cheaply, because the hashes are computed *during* reads
(`CPackedStore::RegisterFileTracker`, `basefilesystem.cpp:1328`) rather than after. If
multiplayer is ever in scope, leave a note where the read path would need to tee into a
hasher.

---

## What "enough filesystem to boot" means

The first milestone is not the whole module. It is: **given a `-game` argument, find and
parse `gameinfo.txt`, build the search path list, mount the VPKs, and read a file out of
one.** That's `filesystem_init.cpp` plus the read path, and it is a genuinely small
target.

The sequence, from `filesystem_init.cpp`:

1. **Locate `gameinfo.txt`** (`LocateGameInfoFile`, `:1023`). With `-game` given, look in
   that directory. Otherwise check `-vproject`/the `VProject` env var, then bubble up
   parent directories (`TryLocateGameInfoFile`, `:964`), with a `content/` → `game/`
   remapping attempt for authoring trees. Most of this is tools-oriented; the engine sets
   `m_bOnlyUseDirectoryName` and takes the first branch.
2. **Parse it** (`LoadGameInfoFile`, `:518`). It's a KeyValues file; the port needs
   `FileSystem.SearchPaths` out of it. Missing `FileSystem` or `SearchPaths` is a hard
   error.
3. **Walk the `SearchPaths` block in order** (`FileSystem_LoadSearchPaths`, `:723`). Each
   entry is `<pathID> <location>`, where the location may carry a prefix token:
   - `|gameinfo_path|` — relative to the directory containing `gameinfo.txt`.
   - `|all_source_engine_paths|` — relative to the base dir, plus (for hldsupdatetool
     dedicated servers only, `:775`) a second `../` copy. That second case is dead for us.
   - otherwise — relative to the base dir.
   Note the key may repeat, and **order within the block is the search order.**
4. **Per-entry expansion** (`FileSystem_AddLoadedSearchPath`, `:635`). For `game`-tagged
   entries specifically, and *before* adding the entry itself:
   - add `<path>_<language>` and `<basedir>/localization/<gamedir>_<language>` if a
     language is set (`AddLanguageGameDir`, `:272`),
   - add `<path>_lv` for low-violence builds (always false on POSIX — `IsLowViolenceBuild`
     is `return false` for POSIX at `:628`, so this is dead unless `-lv` is passed),
   - add `<path>_tempcontent` under `-tempcontent`,
   - tag the **first** `game` entry as `MOD` and record it as the mod path,
   - add `<path>/bin` as `GAMEBIN`.
5. **Then** the fixed extras: `<basedir>/platform` as `GAME` (`:820`), the `CONTENT`
   mirror (`:824-866`, tools-only), the by-request-only marks (`:865-872`), and finally
   the mod path as `DEFAULT_WRITE_PATH` (`:877`).
6. `FileSystem_SetBasePaths` (`:1397`) adds the executable directory as
   `EXECUTABLE_PATH`.

**One live bug to not reproduce:** the `CONTENT` mirror construction at `:828-834` uses
literal backslashes — `V_strrchr( szContentRoot, '\\' )` then `V_strncat(...,
"\\content", ...)`. On POSIX the `strrchr` finds nothing, so the parent-directory
truncation silently doesn't happen, and then a literal `\content` is appended to the
path. The resulting `CONTENT` search paths are garbage on Linux. It goes unnoticed
because `content` is marked by-request-only and only tools ask for it. The Rust port
should either implement `CONTENT` correctly or omit it (I'd omit it — it's an authoring
feature and we are not porting Hammer).

---

## Disposition table

| Piece | Disposition |
|---|---|
| Search path list + path IDs + resolution order | **Port faithfully.** This is the module's real content |
| `gameinfo.txt` parsing and search path construction | **Port faithfully**, minus Steam/console/tools branches |
| VPK reading | **Port faithfully** (format fixed), **modernize the parser** (`binrw`/`deku`) |
| `.bsp` embedded pak lump mounting | **Port** — required for map loading |
| Plain zip reading (`zip_utils.cpp`) | **Replace** with the `zip` crate where the format allows |
| Byte-level `FS_*` wrappers | **Replace** with `std::fs`/`std::io` |
| POSIX case-insensitive fallback | **Redesign** — per-mount folded index, both platforms |
| `FindFirst`/`FindNext` + `linux_support.cpp` | **Replace** — iterator over mounts; `walkdir`/`globset` |
| Async (`basefilesystemasync.cpp`) | **Defer**, then redesign. Do not port the callback API |
| `IAsyncFileSystem` (`filesystemasync.*`, `iasyncfilesystem.h`) | **Delete** — dead code, no callers |
| `QueuedLoader.cpp` | **Defer** — optimization only |
| `filetracker.cpp` + whitelist/CRC interface | **Delete** for single-player Portal 2; note the tee point |
| `LoadModule`/`UnloadModule` | **Delete** — no `dlopen` in a static binary |
| `KeyValuesPreloadType_t` / compiled KeyValues | **Delete** — a load-time optimization for a format we're replacing anyway |
| `filesystem_steam.cpp`, `MountSteamContent`, Steam env vars | **Delete** — not built, `IsSteam()` is always false |
| `XboxInstaller.cpp`, `filegroup.cpp`, DLC/DVD/`_X360`/`_PS3` | **Delete** |
| VPK *writing* | **Delete** — tooling, not runtime |

---

## The Rust design

Per the polarity rule, this starts from the domain model, not from `IFileSystem`.

The domain model is: **an ordered list of mounts, each of which can answer "do you have
this relative path, and can I read it as bytes", filtered by a role tag.** Everything
else in `IFileSystem` is either an optimization, a platform workaround, or a different
subsystem that ended up here.

A sketch, to be refined when it's written rather than treated as settled:

```rust
/// Where a search path sits in the game's content layout.
/// Replaces the interned path-ID strings and the `//pathid/file` syntax.
pub enum PathId { Game, Mod, GameBin, Platform, ExecutablePath, WritePath }

/// One mounted source of files, searched in list order.
trait Mount {
    fn open(&self, path: &RelPath) -> Option<io::Result<Box<dyn ReadSeek + Send>>>;
    fn entries(&self, dir: &RelPath) -> Option<Vec<Entry>>;
}
// implementors: DirMount, VpkMount, BspPakMount, ZipMount

pub struct Vfs { mounts: Vec<(PathId, Box<dyn Mount>)>, write_root: PathBuf }

impl Vfs {
    pub fn from_gameinfo(game_dir: &Path, base_dir: &Path) -> Result<Self, VfsError>;

    pub fn read(&self, path: &RelPath) -> Result<Vec<u8>, VfsError>;
    pub fn open(&self, path: &RelPath) -> Result<Box<dyn ReadSeek + Send>, VfsError>;
    pub fn exists(&self, path: &RelPath) -> bool;
    pub fn list(&self, dir: &RelPath, pat: &Glob) -> impl Iterator<Item = Entry> + '_;

    /// Restrict to one role — replaces the path-ID argument threaded through
    /// every single method in `IFileSystem`.
    pub fn scoped(&self, id: PathId) -> ScopedVfs<'_>;
}
```

Decisions embedded in that sketch, each a deliberate departure:

- **`Read + Seek` trait objects, not `FileHandle_t` (`void*`).** Lifetime is enforced;
  `Close` disappears into `Drop`.
- **`Result` with a real error enum**, not `bool`/sentinel/out-param.
- **Path IDs are an enum**, not interned strings — no `g_PathIDTable`, no `CUtlSymbol`,
  no `//pathid/file` parsing, no "two path IDs specified" warning.
- **By-request-only becomes explicit scoping.** `Vfs::read` searches content mounts;
  `vfs.scoped(PathId::GameBin).read(...)` searches only that. The flag existed because
  path IDs were optional strings; with an enum the distinction is in the API shape.
- **VPKs are ordinary mounts in the ordered list**, not a global list consulted first.
  This changes behavior — see the quirk above — and needs to be a stated, verified
  decision.
- **`BSP` becomes a real mount** pushed at map load and popped at map unload, not a magic
  path-ID string with special filtering.
- **One write root.** `DEFAULT_WRITE_PATH` was a search path only because everything was
  a search path; writes never actually searched.
- **Case folding at mount time**, uniformly on both platforms.
- **No `IAppSystem`**: `Vfs::from_gameinfo(...) -> Result<Self, _>` returns a
  fully-initialized value. No `Connect`/`Init`/`Shutdown`/`Disconnect`.
- **Ownership, not a singleton.** The engine owns a `Vfs` and passes `&Vfs` down. No
  `g_pFullFileSystem`. Fan-in is large (434 files in `legacy/` mention `IFileSystem`) but
  the vast majority are `game/client`, `game/server`, and tools that are being rewritten
  or dropped anyway — this is not a reason to keep a global.

Crates worth using rather than porting: `binrw` or `deku` for the VPK directory, `zip`
for plain zips, `walkdir` + `globset` for iteration, `thiserror` for the error enum,
`memmap2` if VPK reads want it (measure first).

`KeyValues` deserves its own decision, and it arrives here first because `gameinfo.txt`
is a KeyValues file. The format is fixed for Valve-authored files (`gameinfo.txt`,
`.vmt`, soundscapes) so a real parser is needed — `nom` or a hand-written one, small
either way. Don't reach for `serde` for the *format*; do use it for anything we author
ourselves.

---

## Staged plan

Each stage is independently verifiable, which matters because there's no running hybrid
to test against.

1. **KeyValues reader.** Enough to parse `gameinfo.txt`: nested blocks, quoted and
   unquoted tokens, `//` comments, duplicate keys preserved in order. Unit-testable
   against fixtures copied from a real install.
2. **`gameinfo.txt` → search path list.** Port `FileSystem_LoadSearchPaths`' ordering
   rules including the `|gameinfo_path|`/`|all_source_engine_paths|` tokens, the
   first-`game`-is-`MOD` rule, and the `GAMEBIN`/`PLATFORM`/`WritePath` extras. Output is
   a printable list — compare it directly against `PrintSearchPaths()` output from a
   stock build. **This is the highest-value verification opportunity in the whole
   module**; take it.
3. **`DirMount` + case-folded index.** Read a loose file through the search path list.
4. **`VpkMount`.** Parse the v1/v2 directory, read a file spanning numbered archives, and
   handle the embedded-chunk case. Verify against a real `pak01_dir.vpk` by comparing
   extracted bytes with a known-good VPK extractor.
5. **Iteration** (`list`) across mounts with dedup — needed for map lists and content
   enumeration.
6. **`BspPakMount`**, when map loading lands. Not before.
7. **Async**, when a consumer exists to measure. Not before.

Stages 1–4 are what `PORTING.md` means by "enough to read `gameinfo.txt` and mount VPKs",
and they're a realistic first landing.

---

## Open questions

- **Does VPK-wins-always get preserved?** My recommendation is no — make VPKs ordered
  mounts — but this changes observable behavior for loose-file overrides and should be
  checked against a real Portal 2 install before it's locked in.
- **What is Portal 2's actual content layout?** The `pakNN_dir.vpk` scan, the `update/`
  directory, and the `csgo_dlc%d` DLC scan are all CS:GO-shaped. Portal 2's real
  directory and VPK naming can't be determined from this tree — there are no assets and
  no `gameinfo.txt` anywhere in the repo. **Get a real install before writing stage 2.**
- **Which `gameinfo.txt` keys beyond `FileSystem.SearchPaths` matter?** `SteamAppId`,
  `ToolsAppId`, `GameData`, and `singleplayer_only` are read elsewhere in the tree; worth
  a sweep once a real Portal 2 `gameinfo.txt` is in hand.
- **How is the language for `AddLanguageGameDir` determined?** `initInfo.m_pLanguage` is
  set by the caller, not by `filesystem_init.cpp`; trace the engine side before deciding
  whether localized search paths matter for a first boot.
- **Does anything still need plain-zip support at runtime**, or is it only `.bsp` pak
  lumps and VPKs? If the latter, `zip_utils.cpp`'s replacement scope shrinks a lot.
- **Async model**: deferred by design. Revisit when map loading exists.

---

## Notes for whoever picks this up

**Some `legacy/` files are Latin-1, not UTF-8, and this silently breaks grep.** Several
files carry a `©` in the Valve copyright header (`filesystem/filesystemasync.h` is one).
The default `grep` in this environment skips such files *without an error*, returning
zero matches rather than failing. I got a false "this class doesn't exist anywhere"
result from exactly this, and an initial fan-in count of 223 files that was really 434.
**Prefix tree-wide searches with `LC_ALL=C` and pass `-a`.** This affects
`codebase-memory-mcp`'s `search_code` fallback path too — treat negative results in this
module with suspicion and confirm with `check_index_coverage`.

`basefilesystem.cpp` is 9,475 lines and near-certainly has unparsed ranges in the graph
index; check coverage before trusting a negative structural result about it.

Read in this order: `filesystem_init.cpp:723` (`FileSystem_LoadSearchPaths`) →
`filesystem_init.cpp:635` (`FileSystem_AddLoadedSearchPath`) → `basefilesystem.cpp:2842`
(`AddSearchPath`) → `basefilesystem.cpp:4204` (`FindFileInSearchPaths`) →
`basefilesystem.cpp:4131` (`FindFile`). That's the entire boot-critical path in about
500 lines of reading, and everything else in the module hangs off it.
