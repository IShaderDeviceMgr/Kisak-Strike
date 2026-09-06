//! The virtual filesystem: search paths, mounts, and reads.
//!
//! Replaces `filesystem/basefilesystem.cpp` (`CBaseFileSystem`),
//! `filesystem/filesystem_stdio.cpp`, `vpklib/packedstore.cpp` and
//! `public/filesystem_init.cpp`. See `portdocs/FILESYSTEM.md` for the scoping
//! work behind it.
//!
//! `IFileSystem` (`public/filesystem.h:549`) declares roughly 180 pure-virtual
//! methods. Per `PORTING.md`'s polarity rule this API is designed from the
//! domain model instead, which is small: **an ordered list of mounts, each of
//! which can answer "do you have this relative path, and can I read it as
//! bytes", filtered by a role tag.** Everything else in `IFileSystem` is an
//! optimization, a platform workaround, or a different subsystem that ended up
//! there.
//!
//! Deliberate departures, each recorded in `portdocs/FILESYSTEM.md`:
//!
//! * **No `IAppSystem`.** [`Vfs::mount_game`] returns a fully-initialized value;
//!   there is no `Connect`/`Init`/`Shutdown`/`Disconnect` and no reachable
//!   half-constructed state.
//! * **No global.** The engine owns a `Vfs` and passes `&Vfs` down. There is no
//!   `g_pFullFileSystem`.
//! * **Path IDs are an enum**, so there is no `g_PathIDTable`, no `CUtlSymbol`
//!   interning, and no `//pathid/file` string syntax to parse (or for a stray
//!   `V_FixSlashes` to mangle — the thing Valve annotated `FIXME: Pain!` at
//!   `basefilesystem.cpp:4309`).
//! * **By-request-only becomes scoping.** `MarkPathIDByRequestOnly` existed
//!   because path IDs were optional strings; [`Vfs::scoped`] makes the same
//!   distinction structural.
//! * **`FileHandle_t` (`void *`) becomes `Box<dyn ReadSeek>`**, so `Close`
//!   disappears into `Drop`.
//!
//! # Not implemented yet
//!
//! * **Async.** `basefilesystemasync.cpp`'s callback API with manual buffer
//!   ownership should not survive contact with Rust, and nothing on the boot
//!   path needs it. Deferred deliberately until a consumer exists to measure —
//!   picking a concurrency model first is the wrong order.
//! * **`.bsp` embedded pak lump mounts.** Needed for map loading, not before.
//!   [`Mount`] is the seam they will slot into.
//! * **`sv_pure` file tracking.** Dropped for a single-player Portal 2 target.
//!   If multiplayer ever comes into scope, note that the original computes
//!   hashes *during* reads (`CPackedStore::RegisterFileTracker`,
//!   `basefilesystem.cpp:1328`) rather than after, so the tee point is inside
//!   [`mount::vpk::VpkMount`]'s read path.

// This subsystem has landed ahead of its consumers: nothing calls `read`,
// `open`, `list` or `scoped` yet because the engine that will is still C++ in
// `legacy/`. Without this, ~25 dead-code warnings would drown out real ones.
// Remove it once `src/engine/` exists.
#![allow(dead_code)]

pub mod error;
pub mod gameinfo;
pub mod keyvalues;
pub mod mount;
pub mod path;

pub use error::{Result, VfsError};
pub use gameinfo::{GameInfo, SearchPathOptions};
pub use mount::{Entry, ReadSeek};
pub use path::RelPath;

use gameinfo::{PlannedPath, GAMEINFO_FILENAME};
use mount::{dir::DirMount, vpk::VpkMount, Mount};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Where a search path sits in the game's content layout.
///
/// Replaces the interned path-ID strings established across
/// `public/filesystem_init.cpp`.
///
/// `CONTENT` is absent: it is an authoring-tree feature for tools we are not
/// porting, and its construction is broken on POSIX (see [`gameinfo`]).
/// `DEFAULT_WRITE_PATH` is absent because there is one write root, reachable
/// through [`Vfs::write_root`], rather than a search path that never searched.
/// `BSP` was never a path ID at all — it is a lookup-time filter accepting only
/// the current map's embedded pak (`basefilesystem.h`, `FilterByPathID`) — and
/// becomes a real mount pushed at map load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathId {
    /// Everything readable as game content.
    Game,
    /// The active mod directory — the first `game`-tagged path only.
    Mod,
    /// `<mod>/bin`.
    GameBin,
    /// `<basedir>/platform`.
    Platform,
    /// The directory holding the executable.
    ExecutablePath,
}

impl PathId {
    /// Whether an unscoped lookup skips this role.
    ///
    /// `MarkPathIDByRequestOnly` (`filesystem_init.cpp:865-872`) marks
    /// `content`, `executable_path`, `gamebin` and `mod`. It is a correctness
    /// property, not only an optimization: a bare lookup for `bin/foo` must not
    /// find `<mod>/bin/foo`.
    pub fn is_by_request_only(self) -> bool {
        matches!(self, PathId::Mod | PathId::GameBin | PathId::ExecutablePath)
    }
}

struct MountEntry {
    path_id: PathId,
    mount: Arc<dyn Mount>,
}

/// An ordered list of mounts, searched front to back.
pub struct Vfs {
    mounts: Vec<MountEntry>,
    /// Where writes land — the mod directory. `DEFAULT_WRITE_PATH`.
    write_root: PathBuf,
    /// Non-fatal problems encountered while mounting.
    warnings: Vec<String>,
    /// VPKs already parsed, keyed by dir-file path.
    ///
    /// The mod directory is added twice — once as `MOD`, once as `GAME` — and
    /// the original's VPK scan runs on every `AddSearchPath` call, so the same
    /// archive is discovered more than once. Sharing one parsed copy matters:
    /// a shipping `pak01_dir.vpk` carries tens of megabytes of directory blob
    /// plus a six-figure entry map, and parsing it per search path would
    /// duplicate all of it.
    vpk_cache: HashMap<PathBuf, Arc<VpkMount>>,
    /// The current map's `LUMP_PAKFILE`, searched **before** every other mount.
    ///
    /// A field of its own rather than an entry in `mounts` because its
    /// lifetime is a map's rather than the process's, and because it is set
    /// while the rest of the engine holds the `Vfs` by shared reference: the
    /// map loader has a `&Vfs`, not a `&mut Vfs`, and threading mutability
    /// through every subsystem that reads a file to accommodate the one mount
    /// that comes and goes would be the tail wagging the dog. See
    /// [`set_map_pak`](Vfs::set_map_pak).
    map_pak: RwLock<Option<MountEntry>>,
}

impl std::fmt::Debug for Vfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vfs")
            .field("mounts", &self.mounts.len())
            .field("write_root", &self.write_root)
            .finish()
    }
}

/// How VPKs are ordered relative to loose files. See [`Vfs::mount_game`].
const VPKS_AFTER_LOOSE_FILES: bool = true;

/// Highest `pakNN_dir.vpk` probed when scanning a directory
/// (`basefilesystem.cpp:2954` loops `i < 99`, starting at 1).
const MAX_PAK_INDEX: u32 = 98;

impl Vfs {
    /// Reads `<game_dir>/gameinfo.txt`, builds the search path list, and mounts
    /// every directory and VPK it names.
    ///
    /// This is `FileSystem_LoadSearchPaths` plus `CBaseFileSystem::AddSearchPath`'s
    /// VPK discovery, and it is what `PORTING.md` means by "enough filesystem to
    /// boot".
    ///
    /// A search path that does not exist on disk is not an error — `gameinfo.txt`
    /// routinely lists optional DLC and localization directories.
    ///
    /// # VPK ordering — a deliberate behavior change
    ///
    /// In the original, VPKs are **not** entries in `m_SearchPaths`. They live
    /// in a separate global list (`m_VPKFiles`, `basefilesystem.h:1153`) that
    /// `FindFile` re-scans in full on *every* search path iteration
    /// (`basefilesystem.cpp:4166-4185`). Two consequences: VPK content wins over
    /// loose files unconditionally, wherever the VPK was mounted relative to the
    /// directory in search order; and the scan is O(paths x VPKs) returning the
    /// same answer every time.
    ///
    /// Here VPKs are ordinary mounts in the one ordered list, placed
    /// immediately after the directory they were found in — so a loose file
    /// overrides the VPK content beside it, which is what the search path order
    /// implies and what people expect. `portdocs/FILESYSTEM.md` recommends this
    /// and flags that it should be checked against a real Portal 2 install
    /// before being locked in; that check has **not** happened yet, and
    /// [`VPKS_AFTER_LOOSE_FILES`] is the single switch to flip if it turns out
    /// content depends on the original behavior.
    pub fn mount_game(
        game_dir: &Path,
        base_dir: &Path,
        options: &SearchPathOptions,
    ) -> Result<Self> {
        let info = GameInfo::load(game_dir)?;
        let plan = gameinfo::plan_search_paths(&info, game_dir, base_dir, options);

        let write_root = plan.mod_dir.clone().unwrap_or_else(|| {
            // No `game`-tagged path means no mod directory. The original leaves
            // DEFAULT_WRITE_PATH unset in that case and writes go nowhere
            // useful; falling back to the gameinfo directory is at least a real
            // location.
            game_dir.to_path_buf()
        });

        let mut vfs = Vfs {
            mounts: Vec::new(),
            write_root,
            warnings: plan.warnings,
            vpk_cache: HashMap::new(),
            map_pak: RwLock::new(None),
        };

        for PlannedPath { path_id, dir } in plan.paths {
            vfs.add_search_path(path_id, &dir);
        }

        Ok(vfs)
    }

    /// Adds one directory and any `pakNN_dir.vpk` archives inside it.
    ///
    /// `CBaseFileSystem::AddSearchPath` (`basefilesystem.cpp:2842`) minus its
    /// CS:GO-specific behavior, which `portdocs/FILESYSTEM.md` singles out as
    /// the concrete CS:GO-shaped default that matters in this module: a
    /// hardcoded `const char *pGameName = "csgo"` (`:2851`) gating a sibling
    /// `update/` mount and a `csgo_dlc1..99` scan, plus the `COMPAT:` path-ID
    /// prefix mapping to `csgo/pakxv_<name>.vpk`. None of that is portable to
    /// Portal 2 as written, and Portal 2's real update/DLC layout cannot be
    /// determined from this tree — there are no assets and no `gameinfo.txt`
    /// anywhere in the repo. It belongs in configuration once a real install is
    /// available to check against; until then this mounts exactly what
    /// `gameinfo.txt` names.
    pub fn add_search_path(&mut self, path_id: PathId, dir: &Path) {
        let dir_mount: Arc<dyn Mount> = Arc::new(DirMount::new(dir));

        if !VPKS_AFTER_LOOSE_FILES {
            self.push_vpks(path_id, dir);
        }
        self.mounts.push(MountEntry {
            path_id,
            mount: dir_mount,
        });
        if VPKS_AFTER_LOOSE_FILES {
            self.push_vpks(path_id, dir);
        }
    }

    /// Scans `dir` for `pak01_dir.vpk`, `pak02_dir.vpk`, … stopping at the
    /// first gap, and mounts each.
    fn push_vpks(&mut self, path_id: PathId, dir: &Path) {
        for index in 1..=MAX_PAK_INDEX {
            let candidate = dir.join(format!("pak{index:02}_dir.vpk"));
            if !candidate.is_file() {
                break;
            }
            let cached = match self.vpk_cache.get(&candidate) {
                Some(vpk) => Some(Arc::clone(vpk)),
                None => match VpkMount::open(&candidate) {
                    Ok(vpk) => {
                        let vpk = Arc::new(vpk);
                        self.vpk_cache.insert(candidate.clone(), Arc::clone(&vpk));
                        Some(vpk)
                    }
                    Err(e) => {
                        self.warnings
                            .push(format!("skipping {}: {e}", candidate.display()));
                        None
                    }
                },
            };
            if let Some(vpk) = cached {
                self.mounts.push(MountEntry {
                    path_id,
                    mount: vpk,
                });
            }
        }
    }

    /// Mounts an already-opened source under `path_id`, at the end of the list.
    pub fn push_mount(&mut self, path_id: PathId, mount: Arc<dyn Mount>) {
        self.mounts.push(MountEntry { path_id, mount });
    }

    /// Sets — or with `None` clears — the map's embedded pak file.
    ///
    /// `AddSearchPath( <map>.bsp, "GAME", PATH_ADD_TO_HEAD )` and the matching
    /// `RemoveSearchPath` (`engine/modelloader.cpp:4229` and `:6269`), which is
    /// how the engine makes a map's own generated content visible for exactly
    /// as long as the map is loaded.
    ///
    /// **At the head**, so a mapper's embedded copy of an asset wins over the
    /// shipped one, which is what embedding it means. There is only ever one,
    /// so setting replaces.
    ///
    /// Takes `&self`: see [`map_pak`](Vfs::map_pak). A caller that already has
    /// `&mut Vfs` may still call it.
    pub fn set_map_pak(&self, pak: Option<(PathId, Arc<dyn Mount>)>) {
        let entry = pak.map(|(path_id, mount)| MountEntry { path_id, mount });
        match self.map_pak.write() {
            Ok(mut slot) => *slot = entry,
            // A poisoned lock means a panic while a *different* thread held it,
            // which cannot lose data here: the value is one `Option` replaced
            // wholesale, never mutated in place.
            Err(poisoned) => *poisoned.into_inner() = entry,
        }
    }

    /// Non-fatal problems encountered while mounting.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Where writes land — `DEFAULT_WRITE_PATH`, i.e. the mod directory.
    pub fn write_root(&self) -> &Path {
        &self.write_root
    }

    /// Resolves a relative path against the write root.
    pub fn write_path(&self, path: &str) -> Result<PathBuf> {
        let rel = RelPath::new(path)?;
        Ok(self.write_root.join(rel.as_str()))
    }

    /// The search path list, in order, for diagnostics.
    ///
    /// `CBaseFileSystem::PrintSearchPaths`. `portdocs/FILESYSTEM.md` calls
    /// comparing this against a stock build "the highest-value verification
    /// opportunity in the whole module".
    pub fn search_paths(&self) -> impl Iterator<Item = (PathId, String)> + '_ {
        let pak = self
            .map_pak
            .read()
            .ok()
            .and_then(|slot| slot.as_ref().map(|e| (e.path_id, e.mount.describe())));
        pak.into_iter()
            .chain(self.mounts.iter().map(|m| (m.path_id, m.mount.describe())))
    }

    /// Restricts lookups to one role — replaces the path-ID argument threaded
    /// through every method of `IFileSystem`.
    pub fn scoped(&self, path_id: PathId) -> ScopedVfs<'_> {
        ScopedVfs {
            vfs: self,
            path_id: Some(path_id),
        }
    }

    fn all(&self) -> ScopedVfs<'_> {
        ScopedVfs {
            vfs: self,
            path_id: None,
        }
    }

    /// Reads a whole file, searching every content mount in order.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.all().read(path)
    }

    /// Opens a seekable stream, searching every content mount in order.
    pub fn open(&self, path: &str) -> Result<Box<dyn ReadSeek>> {
        self.all().open(path)
    }

    /// Whether the path resolves in any content mount.
    pub fn exists(&self, path: &str) -> bool {
        self.all().exists(path)
    }

    /// Immediate children of `dir`, merged across mounts.
    pub fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        self.all().list(dir)
    }
}

/// A [`Vfs`] view restricted to one [`PathId`], or to the default content roles.
#[derive(Clone, Copy)]
pub struct ScopedVfs<'a> {
    vfs: &'a Vfs,
    path_id: Option<PathId>,
}

impl ScopedVfs<'_> {
    /// Whether a lookup in this scope should consult `entry`.
    fn accepts(&self, entry: &MountEntry) -> bool {
        match self.path_id {
            Some(want) => entry.path_id == want,
            // An unscoped lookup skips by-request-only roles.
            None => !entry.path_id.is_by_request_only(),
        }
    }

    fn mounts(&self) -> impl Iterator<Item = &MountEntry> {
        self.vfs.mounts.iter().filter(|m| self.accepts(m))
    }

    /// The map's pak lump, if there is one and this scope may see it.
    ///
    /// Returned by value rather than by reference because it lives behind a
    /// lock; the `Arc` clone is one atomic and the guard is released before
    /// anything reads a file.
    fn map_pak(&self) -> Option<Arc<dyn Mount>> {
        let slot = self
            .vfs
            .map_pak
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = slot.as_ref()?;
        self.accepts(entry).then(|| Arc::clone(&entry.mount))
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let rel = RelPath::new(path)?;
        if let Some(result) = self.map_pak().and_then(|pak| pak.read(&rel)) {
            return result;
        }
        for entry in self.mounts() {
            if let Some(result) = entry.mount.read(&rel) {
                return result;
            }
        }
        Err(VfsError::NotFound {
            path: rel.as_str().to_string(),
        })
    }

    pub fn open(&self, path: &str) -> Result<Box<dyn ReadSeek>> {
        let rel = RelPath::new(path)?;
        if let Some(result) = self.map_pak().and_then(|pak| pak.open(&rel)) {
            return result;
        }
        for entry in self.mounts() {
            if let Some(result) = entry.mount.open(&rel) {
                return result;
            }
        }
        Err(VfsError::NotFound {
            path: rel.as_str().to_string(),
        })
    }

    pub fn exists(&self, path: &str) -> bool {
        let Ok(rel) = RelPath::new(path) else {
            return false;
        };
        if self.map_pak().is_some_and(|pak| pak.contains(&rel)) {
            return true;
        }
        self.mounts().any(|e| e.mount.contains(&rel))
    }

    /// Immediate children of `dir`, merged across mounts.
    ///
    /// Earlier mounts win on a name collision, matching read order. Comparison
    /// is case-insensitive, so a directory present in both a VPK and a loose
    /// directory is listed once.
    pub fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let rel = if dir.is_empty() || dir == "/" || dir == "." {
            None
        } else {
            Some(RelPath::new(dir)?)
        };

        let mut seen = std::collections::HashSet::new();
        let mut merged = Vec::new();
        let pak = self.map_pak();
        for mount in pak.iter().map(Arc::as_ref).chain(
            self.mounts()
                .map(|e| e.mount.as_ref())
                .collect::<Vec<_>>()
                .into_iter(),
        ) {
            let mut batch = Vec::new();
            mount.list(rel.as_ref(), &mut batch);
            for item in batch {
                if seen.insert(item.name.to_ascii_lowercase()) {
                    merged.push(item);
                }
            }
        }
        Ok(merged)
    }
}

/// Locates the game directory named by a `-game` argument.
///
/// `LocateGameInfoFile` (`filesystem_init.cpp:1023`) with `m_bOnlyUseDirectoryName`
/// set, which is the branch the engine takes. The `-vproject` / `VProject`
/// environment variable path and the parent-directory bubble-up are for tools
/// and are not ported.
pub fn locate_game_dir(base_dir: &Path, game_arg: &str) -> PathBuf {
    let candidate = Path::new(game_arg);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base_dir.join(candidate)
    }
}

/// Whether a directory looks like a game directory.
pub fn has_gameinfo(dir: &Path) -> bool {
    dir.join(GAMEINFO_FILENAME).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!("kisak-vfs-test-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn write(&self, rel: &str, bytes: &[u8]) {
            let full = self.0.join(rel);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::File::create(&full).unwrap().write_all(bytes).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const GAMEINFO: &str = r#"
"GameInfo"
{
	game	"Portal 2"
	FileSystem
	{
		SteamAppId	620
		SearchPaths
		{
			Game	|gameinfo_path|.
			Game	|all_source_engine_paths|portal2_dlc1
		}
	}
}
"#;

    /// A minimal install: base/portal2 (the mod) and base/portal2_dlc1.
    fn install(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        tmp.write("portal2/gameinfo.txt", GAMEINFO.as_bytes());
        tmp
    }

    fn mount(tmp: &TempDir) -> Vfs {
        Vfs::mount_game(
            &tmp.path().join("portal2"),
            tmp.path(),
            &SearchPathOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn mounts_and_reads_a_loose_file() {
        let tmp = install("read");
        tmp.write("portal2/cfg/config.cfg", b"bind w +forward");
        let vfs = mount(&tmp);

        assert_eq!(vfs.read("cfg/config.cfg").unwrap(), b"bind w +forward");
        assert!(vfs.exists("cfg/config.cfg"));
        assert!(!vfs.exists("cfg/absent.cfg"));
    }

    #[test]
    fn missing_gameinfo_is_a_clean_error() {
        let tmp = TempDir::new("nogameinfo");
        let err =
            Vfs::mount_game(tmp.path(), tmp.path(), &SearchPathOptions::default()).unwrap_err();
        assert!(matches!(err, VfsError::GameInfoMissing { .. }));
    }

    #[test]
    fn search_order_follows_gameinfo() {
        let tmp = install("order");
        tmp.write("portal2/shared.txt", b"from mod");
        tmp.write("portal2_dlc1/shared.txt", b"from dlc");
        tmp.write("portal2_dlc1/only_dlc.txt", b"dlc only");
        let vfs = mount(&tmp);

        // The mod directory is listed first in gameinfo, so it wins.
        assert_eq!(vfs.read("shared.txt").unwrap(), b"from mod");
        assert_eq!(vfs.read("only_dlc.txt").unwrap(), b"dlc only");
    }

    #[test]
    fn by_request_only_roles_are_skipped_unscoped() {
        let tmp = install("scoping");
        // A bare lookup for `bin/foo` must not find `<mod>/bin/foo`, which is
        // the correctness property MarkPathIDByRequestOnly protects.
        tmp.write("portal2/bin/tier0.so", b"elf");
        let vfs = mount(&tmp);

        assert!(!vfs.exists("tier0.so"), "GAMEBIN leaked into a bare lookup");
        assert!(vfs.scoped(PathId::GameBin).exists("tier0.so"));
        assert_eq!(
            vfs.scoped(PathId::GameBin).read("tier0.so").unwrap(),
            b"elf"
        );

        // ...but it is still reachable through the GAME path as `bin/tier0.so`,
        // because the mod directory itself is a GAME path.
        assert!(vfs.exists("bin/tier0.so"));
    }

    #[test]
    fn scoping_to_mod_searches_only_the_mod_directory() {
        let tmp = install("modscope");
        tmp.write("portal2_dlc1/only_dlc.txt", b"x");
        let vfs = mount(&tmp);

        assert!(vfs.exists("only_dlc.txt"));
        assert!(!vfs.scoped(PathId::Mod).exists("only_dlc.txt"));
    }

    #[test]
    fn not_found_reports_the_normalized_path() {
        let tmp = install("notfound");
        let vfs = mount(&tmp);
        match vfs.read("Materials\\Metal\\..\\wall.vmt").unwrap_err() {
            VfsError::NotFound { path } => assert_eq!(path, "Materials/wall.vmt"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn traversal_out_of_the_mount_is_rejected() {
        let tmp = install("traversal");
        tmp.write("secret.txt", b"should not be reachable");
        let vfs = mount(&tmp);
        assert!(matches!(
            vfs.read("../secret.txt"),
            Err(VfsError::InvalidPath { .. })
        ));
        assert!(!vfs.exists("../secret.txt"));
    }

    #[test]
    fn write_root_is_the_mod_directory() {
        let tmp = install("write");
        let vfs = mount(&tmp);
        assert_eq!(vfs.write_root(), tmp.path().join("portal2"));
        assert_eq!(
            vfs.write_path("cfg/config.cfg").unwrap(),
            tmp.path().join("portal2/cfg/config.cfg")
        );
    }

    #[test]
    fn search_paths_are_reported_in_order() {
        let tmp = install("print");
        let vfs = mount(&tmp);
        let listed: Vec<_> = vfs.search_paths().collect();

        assert_eq!(listed[0].0, PathId::Mod);
        assert_eq!(listed[1].0, PathId::GameBin);
        assert_eq!(listed[2].0, PathId::Game);
        assert!(listed.last().unwrap().1.ends_with("platform"));
    }

    #[test]
    fn nonexistent_search_paths_are_tolerated() {
        // portal2_dlc1 is named by gameinfo but never created.
        let tmp = install("absent");
        tmp.write("portal2/a.txt", b"ok");
        let vfs = mount(&tmp);
        assert_eq!(vfs.read("a.txt").unwrap(), b"ok");
        assert!(vfs.warnings().is_empty());
    }

    #[test]
    fn lists_across_mounts_without_duplicates() {
        let tmp = install("list");
        tmp.write("portal2/maps/a.bsp", b"");
        tmp.write("portal2/maps/b.bsp", b"");
        tmp.write("portal2_dlc1/maps/b.bsp", b"");
        tmp.write("portal2_dlc1/maps/c.bsp", b"");
        let vfs = mount(&tmp);

        let mut names: Vec<_> = vfs
            .list("maps")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.bsp", "b.bsp", "c.bsp"]);
    }

    #[test]
    fn public_types_are_thread_safe() {
        // Documented in rustdocs/FILESYSTEM.md: the engine holds one `Vfs` and
        // shares `&Vfs` across threads. Asserted rather than assumed, since
        // adding a non-Sync field to any mount would silently break it.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Vfs>();
        assert_send_sync::<RelPath>();
        assert_send_sync::<VfsError>();
        assert_send_sync::<Entry>();
        assert_send_sync::<SearchPathOptions>();
        assert_send_sync::<mount::dir::DirMount>();
        assert_send_sync::<mount::vpk::VpkMount>();

        // Open streams are `Send` but deliberately not `Sync`.
        fn assert_send<T: Send>() {}
        assert_send::<Box<dyn ReadSeek>>();
    }

    #[test]
    fn locates_game_dirs_relative_and_absolute() {
        assert_eq!(
            locate_game_dir(Path::new("/g"), "portal2"),
            PathBuf::from("/g/portal2")
        );
        assert_eq!(
            locate_game_dir(Path::new("/g"), "/elsewhere/portal2"),
            PathBuf::from("/elsewhere/portal2")
        );
    }
}
