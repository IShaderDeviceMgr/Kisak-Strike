//! `gameinfo.txt` — the bootstrap that turns a `-game` argument into an
//! ordered list of search paths.
//!
//! Ported from `public/filesystem_init.cpp`, chiefly `FileSystem_LoadSearchPaths`
//! (`:723`) and `FileSystem_AddLoadedSearchPath` (`:635`). In the original this
//! file is not part of the filesystem library at all — it is compiled into every
//! consumer separately. That distinction is meaningless here, so it is simply
//! part of this module.
//!
//! Deleted along the way, per `portdocs/FILESYSTEM.md`'s disposition table:
//!
//! * **Every `IsSteam()` branch.** `filesystem_steam.cpp` is not in the build,
//!   so `MountSteamContent`, the Steam environment setup, `SetSteamAppUser` and
//!   `GetSteamExtraAppId` are dead on arrival.
//! * **The `CONTENT` search paths** (`filesystem_init.cpp:824-866`). They are an
//!   authoring-tree feature for tools we are not porting, and the construction
//!   is broken on POSIX anyway: it looks for a literal `'\\'` with `V_strrchr`
//!   and appends a literal `"\\content"`, so on Linux the parent-directory
//!   truncation silently does not happen and a `\content` segment is glued onto
//!   the path. It goes unnoticed because `content` is marked by-request-only and
//!   only tools ask for it.
//! * **The hldsupdatetool second copy** of `|all_source_engine_paths|`
//!   (`:775`), which fires only when the executable sits in a directory named
//!   `orangebox`.
//! * **`LocateGameInfoFile`'s parent-directory search and `-vproject` handling.**
//!   The engine sets `m_bOnlyUseDirectoryName` and takes the `-game` branch; the
//!   rest is for tools.

use crate::filesystem::error::{Result, VfsError};
use crate::filesystem::keyvalues;
use crate::filesystem::path::make_absolute;
use crate::filesystem::PathId;
use std::path::{Path, PathBuf};

pub const GAMEINFO_FILENAME: &str = "gameinfo.txt";

const GAMEINFO_PATH_TOKEN: &str = "|gameinfo_path|";
const ALL_SOURCE_ENGINE_PATHS_TOKEN: &str = "|all_source_engine_paths|";

/// The parts of `gameinfo.txt` this port reads.
///
/// Other keys exist (`GameData`, `ToolsAppId`, `singleplayer_only`, the
/// `Game_LowViolence` variants) and are read elsewhere in the original tree;
/// they are left unparsed until something needs them.
#[derive(Debug, Clone)]
pub struct GameInfo {
    /// The `game` key — the human-readable title.
    pub title: Option<String>,
    /// `FileSystem.SteamAppId`.
    pub steam_app_id: Option<u32>,
    /// `FileSystem.SearchPaths`, verbatim and in order.
    pub search_paths: Vec<SearchPathSpec>,
}

/// One raw `<pathID> <location>` line from the `SearchPaths` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPathSpec {
    pub path_id: String,
    pub location: String,
}

impl GameInfo {
    /// Reads and parses `<dir>/gameinfo.txt`.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(GAMEINFO_FILENAME);
        let text = std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                VfsError::GameInfoMissing {
                    dir: dir.to_path_buf(),
                }
            } else {
                VfsError::io(path.display().to_string(), e)
            }
        })?;
        // gameinfo.txt is nominally ASCII; tolerate stray bytes rather than
        // failing the boot.
        let text = String::from_utf8_lossy(&text);
        Self::parse(&path, &text)
    }

    /// Parses `gameinfo.txt` contents.
    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let doc = keyvalues::parse(&path.display().to_string(), text)?;

        // `ReadKeyValuesFile` parses into a nameless root and then reads
        // through it, so the outer block is found positionally. Mods do rename
        // it away from "GameInfo".
        let root = doc.first_block().ok_or_else(|| VfsError::GameInfoInvalid {
            path: path.to_path_buf(),
            reason: "no top-level block".into(),
        })?;

        let fs = root
            .find_block("FileSystem")
            .ok_or_else(|| VfsError::GameInfoInvalid {
                path: path.to_path_buf(),
                reason: "missing the FileSystem block".into(),
            })?;

        let paths = fs
            .find_block("SearchPaths")
            .ok_or_else(|| VfsError::GameInfoInvalid {
                path: path.to_path_buf(),
                reason: "missing the FileSystem.SearchPaths block".into(),
            })?;

        let search_paths = paths
            .values()
            .map(|(path_id, location)| SearchPathSpec {
                path_id: path_id.to_string(),
                location: location.to_string(),
            })
            .collect();

        Ok(GameInfo {
            title: root.find_string("game").map(str::to_string),
            steam_app_id: fs
                .find_string("SteamAppId")
                .and_then(|s| s.trim().parse().ok()),
            search_paths,
        })
    }
}

/// Knobs that change which search paths get built.
///
/// All of these come from the command line in the original.
#[derive(Debug, Clone, Default)]
pub struct SearchPathOptions {
    /// `initInfo.m_pLanguage`. Set by the caller, not by `filesystem_init.cpp`.
    pub language: Option<String>,
    /// `IsLowViolenceBuild()` — `filesystem_init.cpp:628` returns false
    /// unconditionally on POSIX, so this is only ever true via `-lv`.
    pub low_violence: bool,
    /// `-tempcontent`.
    pub temp_content: bool,
    /// Added last as `EXECUTABLE_PATH` by `FileSystem_SetBasePaths` (`:1397`).
    pub executable_dir: Option<PathBuf>,
}

/// One resolved search path, in the order it should be searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPath {
    pub path_id: PathId,
    pub dir: PathBuf,
}

/// The result of walking `gameinfo.txt`.
#[derive(Debug, Clone)]
pub struct SearchPathPlan {
    pub paths: Vec<PlannedPath>,
    /// The first `game`-tagged directory — the active mod, and the sole write
    /// root. `DEFAULT_WRITE_PATH` is a search path in the original only because
    /// everything was; writes never actually searched.
    pub mod_dir: Option<PathBuf>,
    /// Non-fatal problems worth surfacing (unrecognized path IDs, and so on).
    pub warnings: Vec<String>,
}

/// Maps a `gameinfo.txt` path-ID token onto [`PathId`].
///
/// Compound `game+mod`-style keys appear in Source 2013-era `gameinfo.txt`
/// files. This tree's `FileSystem_LoadSearchPaths` passes the key through
/// verbatim and `AddSearchPath` then `stricmp`s it against `GAME`/`MOD`/
/// `PLATFORM`, so a compound key simply matches nothing there. Splitting on
/// `+` is a deliberate improvement: it costs nothing and means a gameinfo
/// written for a neighbouring Source branch mounts its content instead of
/// silently contributing an untagged path.
fn parse_path_id(raw: &str) -> (Vec<PathId>, Option<String>) {
    let mut ids = Vec::new();
    let mut warning = None;

    for token in raw.split('+') {
        let token = token.trim();
        let id = if token.eq_ignore_ascii_case("game") {
            PathId::Game
        } else if token.eq_ignore_ascii_case("mod") {
            PathId::Mod
        } else if token.eq_ignore_ascii_case("gamebin") {
            PathId::GameBin
        } else if token.eq_ignore_ascii_case("platform") {
            PathId::Platform
        } else if token.eq_ignore_ascii_case("executable_path") {
            PathId::ExecutablePath
        } else if token.eq_ignore_ascii_case("default_write_path")
            || token.eq_ignore_ascii_case("game_write")
            || token.eq_ignore_ascii_case("mod_write")
        {
            // There is one write root and it is the mod directory; these tokens
            // carry no additional information.
            continue;
        } else if token.eq_ignore_ascii_case("content") {
            // Deleted; see the module docs.
            continue;
        } else {
            warning = Some(format!(
                "unrecognized search path ID {token:?}; treating it as GAME"
            ));
            PathId::Game
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    (ids, warning)
}

/// Builds the ordered search path list from a parsed `gameinfo.txt`.
///
/// `gameinfo_dir` is the directory containing `gameinfo.txt`; `base_dir` is the
/// engine's base directory (`-basedir`, normally the install root).
pub fn plan_search_paths(
    info: &GameInfo,
    gameinfo_dir: &Path,
    base_dir: &Path,
    options: &SearchPathOptions,
) -> SearchPathPlan {
    let mut plan = SearchPathPlan {
        paths: Vec::new(),
        mod_dir: None,
        warnings: Vec::new(),
    };
    let mut first_game_path = true;

    for spec in &info.search_paths {
        let (ids, warning) = parse_path_id(&spec.path_id);
        if let Some(w) = warning {
            plan.warnings.push(w);
        }
        if ids.is_empty() {
            continue;
        }

        // Token prefixes decide what the location is relative to.
        let (anchor, location) = if let Some(rest) =
            strip_prefix_ci(&spec.location, GAMEINFO_PATH_TOKEN)
        {
            (gameinfo_dir, rest)
        } else if let Some(rest) = strip_prefix_ci(&spec.location, ALL_SOURCE_ENGINE_PATHS_TOKEN) {
            (base_dir, rest)
        } else {
            (base_dir, spec.location.as_str())
        };

        let full = make_absolute(anchor, location);

        for &id in &ids {
            // `game`-tagged entries expand into several paths, all added
            // *before* the entry itself.
            if id == PathId::Game {
                if let Some(language) = &options.language {
                    add_language_dirs(&mut plan, &full, language);
                }
                if options.low_violence {
                    plan.push(PathId::Game, append_suffix(&full, "_lv"));
                }
                if options.temp_content {
                    plan.push(PathId::Game, append_suffix(&full, "_tempcontent"));
                }
                if first_game_path {
                    first_game_path = false;
                    plan.push(PathId::Mod, full.clone());
                    plan.mod_dir = Some(full.clone());
                }
                plan.push(PathId::GameBin, full.join("bin"));
            }
            plan.push(id, full.clone());
        }
    }

    // `<basedir>/platform`, tagged GAME (`filesystem_init.cpp:820`).
    plan.push(PathId::Game, base_dir.join("platform"));

    if let Some(exe_dir) = &options.executable_dir {
        plan.push(PathId::ExecutablePath, exe_dir.clone());
    }

    plan
}

impl SearchPathPlan {
    fn push(&mut self, path_id: PathId, dir: PathBuf) {
        // The original happily adds the same (path, id) twice; deduplicating
        // keeps the printed list comparable and saves a redundant mount.
        if self
            .paths
            .iter()
            .any(|p| p.path_id == path_id && p.dir == dir)
        {
            return;
        }
        self.paths.push(PlannedPath { path_id, dir });
    }
}

/// `AddLanguageGameDir` (`filesystem_init.cpp:272`).
fn add_language_dirs(plan: &mut SearchPathPlan, full: &Path, language: &str) {
    plan.push(PathId::Game, append_suffix(full, &format!("_{language}")));

    // Also `<prefix>/localization/<gamedir>_<language>` when the path runs
    // through a `game/` component — an authoring-tree layout, so this is
    // normally a no-op for a shipped install.
    let as_str = full.to_string_lossy();
    if let Some(idx) = as_str.find("/game/") {
        let prefix = &as_str[..idx];
        let game_dir = &as_str[idx + "/game/".len()..];
        let candidate = PathBuf::from(format!("{prefix}/localization/{game_dir}_{language}"));
        if candidate.is_dir() {
            plan.push(PathId::Game, candidate);
        }
    }
}

/// Appends a suffix to the final path component (`"<path>_lv"`), which is a
/// sibling directory, not a child.
fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.to_path_buf().into_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn strip_prefix_ci<'a>(haystack: &'a str, prefix: &str) -> Option<&'a str> {
    // `Q_stristr(pLocation, TOKEN) == pLocation` — a case-insensitive
    // "starts with".
    if haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&haystack[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(paths: &[(&str, &str)]) -> GameInfo {
        GameInfo {
            title: None,
            steam_app_id: None,
            search_paths: paths
                .iter()
                .map(|(id, loc)| SearchPathSpec {
                    path_id: id.to_string(),
                    location: loc.to_string(),
                })
                .collect(),
        }
    }

    fn plan_of(paths: &[(&str, &str)]) -> SearchPathPlan {
        plan_search_paths(
            &info(paths),
            Path::new("/g/portal2"),
            Path::new("/g"),
            &SearchPathOptions::default(),
        )
    }

    fn rendered(plan: &SearchPathPlan) -> Vec<String> {
        plan.paths
            .iter()
            .map(|p| format!("{:?} {}", p.path_id, p.dir.display()))
            .collect()
    }

    #[test]
    fn parses_a_realistic_gameinfo() {
        let src = r#"
"GameInfo"
{
	game	"Portal 2"
	FileSystem
	{
		SteamAppId	620
		SearchPaths
		{
			Game	|gameinfo_path|.
			Game	|all_source_engine_paths|portal2_dlc2
			Game	|all_source_engine_paths|portal2
		}
	}
}
"#;
        let gi = GameInfo::parse(Path::new("/g/portal2/gameinfo.txt"), src).unwrap();
        assert_eq!(gi.title.as_deref(), Some("Portal 2"));
        assert_eq!(gi.steam_app_id, Some(620));
        assert_eq!(gi.search_paths.len(), 3);
        assert_eq!(gi.search_paths[0].location, "|gameinfo_path|.");
    }

    #[test]
    fn rejects_gameinfo_without_the_required_blocks() {
        let p = Path::new("gameinfo.txt");
        assert!(GameInfo::parse(p, r#""GameInfo" { game "x" }"#).is_err());
        assert!(GameInfo::parse(p, r#""GameInfo" { FileSystem { } }"#).is_err());
        assert!(GameInfo::parse(p, "").is_err());
        // But an empty SearchPaths block is structurally valid.
        assert!(GameInfo::parse(p, r#""G" { FileSystem { SearchPaths { } } }"#).is_ok());
    }

    #[test]
    fn resolves_the_location_tokens() {
        let plan = plan_of(&[
            ("Game", "|gameinfo_path|."),
            ("Game", "|all_source_engine_paths|portal2_dlc2"),
            ("Game", "bare_relative"),
        ]);
        let dirs: Vec<_> = plan
            .paths
            .iter()
            .filter(|p| p.path_id == PathId::Game)
            .map(|p| p.dir.display().to_string())
            .collect();
        assert!(dirs.contains(&"/g/portal2".to_string()));
        assert!(dirs.contains(&"/g/portal2_dlc2".to_string()));
        assert!(dirs.contains(&"/g/bare_relative".to_string()));
    }

    #[test]
    fn location_tokens_are_case_insensitive() {
        let plan = plan_of(&[("Game", "|GAMEINFO_PATH|.")]);
        assert!(plan
            .paths
            .iter()
            .any(|p| p.dir == Path::new("/g/portal2") && p.path_id == PathId::Game));
    }

    #[test]
    fn first_game_path_becomes_mod_and_the_write_root() {
        let plan = plan_of(&[
            ("Game", "|gameinfo_path|."),
            ("Game", "|all_source_engine_paths|portal2_dlc2"),
        ]);
        assert_eq!(plan.mod_dir.as_deref(), Some(Path::new("/g/portal2")));

        let mods: Vec<_> = plan
            .paths
            .iter()
            .filter(|p| p.path_id == PathId::Mod)
            .collect();
        assert_eq!(mods.len(), 1, "only the first game path is tagged MOD");
        assert_eq!(mods[0].dir, Path::new("/g/portal2"));
    }

    #[test]
    fn mod_and_gamebin_precede_the_game_entry() {
        // FileSystem_AddLoadedSearchPath adds MOD and GAMEBIN before falling
        // through to the AddSearchPath for the entry itself.
        let plan = plan_of(&[("Game", "|gameinfo_path|.")]);
        let order = rendered(&plan);
        let mod_at = order.iter().position(|s| s.starts_with("Mod ")).unwrap();
        let bin_at = order
            .iter()
            .position(|s| s.starts_with("GameBin "))
            .unwrap();
        let game_at = order.iter().position(|s| s == "Game /g/portal2").unwrap();
        assert!(mod_at < game_at);
        assert!(bin_at < game_at);
        assert_eq!(order[bin_at], "GameBin /g/portal2/bin");
    }

    #[test]
    fn platform_is_appended_last_as_game() {
        let plan = plan_of(&[("Game", "|gameinfo_path|.")]);
        assert_eq!(
            plan.paths.last().unwrap(),
            &PlannedPath {
                path_id: PathId::Game,
                dir: PathBuf::from("/g/platform")
            }
        );
    }

    #[test]
    fn executable_path_is_added_when_supplied() {
        let plan = plan_search_paths(
            &info(&[("Game", "|gameinfo_path|.")]),
            Path::new("/g/portal2"),
            Path::new("/g"),
            &SearchPathOptions {
                executable_dir: Some(PathBuf::from("/g/bin")),
                ..Default::default()
            },
        );
        assert_eq!(
            plan.paths.last().unwrap(),
            &PlannedPath {
                path_id: PathId::ExecutablePath,
                dir: PathBuf::from("/g/bin")
            }
        );
    }

    #[test]
    fn optional_expansions_are_siblings_not_children() {
        let plan = plan_search_paths(
            &info(&[("Game", "|gameinfo_path|.")]),
            Path::new("/g/portal2"),
            Path::new("/g"),
            &SearchPathOptions {
                language: Some("french".into()),
                low_violence: true,
                temp_content: true,
                ..Default::default()
            },
        );
        let dirs: Vec<_> = plan
            .paths
            .iter()
            .map(|p| p.dir.display().to_string())
            .collect();
        assert!(dirs.contains(&"/g/portal2_french".to_string()));
        assert!(dirs.contains(&"/g/portal2_lv".to_string()));
        assert!(dirs.contains(&"/g/portal2_tempcontent".to_string()));
    }

    #[test]
    fn language_and_low_violence_are_absent_by_default() {
        let plan = plan_of(&[("Game", "|gameinfo_path|.")]);
        let dirs: Vec<_> = plan
            .paths
            .iter()
            .map(|p| p.dir.display().to_string())
            .collect();
        assert!(!dirs.iter().any(|d| d.ends_with("_lv")));
        assert!(!dirs.iter().any(|d| d.ends_with("_tempcontent")));
    }

    #[test]
    fn non_game_ids_do_not_expand() {
        let plan = plan_of(&[("Platform", "platform")]);
        assert!(plan.mod_dir.is_none());
        assert!(!plan.paths.iter().any(|p| p.path_id == PathId::GameBin));
        assert!(plan
            .paths
            .iter()
            .any(|p| p.path_id == PathId::Platform && p.dir == Path::new("/g/platform")));
    }

    #[test]
    fn compound_path_ids_split() {
        let (ids, warning) = parse_path_id("game+mod");
        assert_eq!(ids, vec![PathId::Game, PathId::Mod]);
        assert!(warning.is_none());

        let (ids, _) = parse_path_id("game+game_write+mod_write");
        assert_eq!(ids, vec![PathId::Game]);
    }

    #[test]
    fn unknown_path_ids_warn_but_still_mount() {
        let plan = plan_of(&[("wibble", "somewhere")]);
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan
            .paths
            .iter()
            .any(|p| p.path_id == PathId::Game && p.dir == Path::new("/g/somewhere")));
    }

    #[test]
    fn content_paths_are_dropped_entirely() {
        let plan = plan_of(&[("content", "somewhere")]);
        assert!(plan.warnings.is_empty());
        assert!(!plan
            .paths
            .iter()
            .any(|p| p.dir == Path::new("/g/somewhere")));
    }

    #[test]
    fn duplicate_entries_collapse() {
        let plan = plan_of(&[
            ("Game", "|all_source_engine_paths|portal2"),
            ("Game", "|all_source_engine_paths|portal2"),
        ]);
        let count = plan
            .paths
            .iter()
            .filter(|p| p.dir == Path::new("/g/portal2") && p.path_id == PathId::Game)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn full_ordering_for_a_portal2_shaped_gameinfo() {
        // The comparison target for `PrintSearchPaths()` against a stock build.
        let plan = plan_of(&[
            ("Game", "|gameinfo_path|."),
            ("Game", "|all_source_engine_paths|portal2_dlc2"),
            ("Game", "|all_source_engine_paths|portal2"),
        ]);
        // The third entry resolves to the same directory as the first, so
        // deduplication drops the GAMEBIN/GAME pair it would have re-added.
        assert_eq!(
            rendered(&plan),
            vec![
                "Mod /g/portal2",
                "GameBin /g/portal2/bin",
                "Game /g/portal2",
                "GameBin /g/portal2_dlc2/bin",
                "Game /g/portal2_dlc2",
                "Game /g/platform",
            ]
        );
    }
}
