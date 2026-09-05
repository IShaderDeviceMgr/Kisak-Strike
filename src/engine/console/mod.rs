//! The console: cvars, commands, the buffer that turns text into them, and the
//! output they print to.
//!
//! `portdocs/ENGINE_CONSOLE.md` is the design; this module is stage 1 of its
//! §8. The original spreads one system across three modules for reasons that
//! are entirely artifacts of the DLL model — `ConVar` lives in `tier1` because
//! every DLL needs the class, the registry in `vstdlib` because exactly one
//! process-wide instance must exist, the policy in `engine` because that is who
//! knows about cheats and servers. In one binary the three are one module.
//!
//! | Layer | Original | Here |
//! |---|---|---|
//! | Objects | `tier1/convar.cpp` | [`cvar`], [`token`] |
//! | Registry | `vstdlib/cvar.cpp` | [`cvar::CvarRegistry`] |
//! | Buffer | `tier1/commandbuffer.cpp` | [`buffer`] |
//! | Policy | `engine/cmd.cpp`, `engine/cvar.cpp` | this file |
//! | Output | `engine/console.cpp` | [`log`] |
//!
//! # The two seams
//!
//! This module names no engine type and depends on `std` alone, which is what
//! lets it be constructed, driven and asserted on without a window, a GPU or a
//! mounted filesystem. Two traits buy that:
//!
//! - [`CommandTarget`] — **a cvar set is handled here; a command is handed
//!   back out.** Setting `fps_max 60` needs nothing but the cvar. Running `map
//!   sp_a1_intro1` needs `&mut Host`. So dispatch resolves aliases and cvars
//!   itself and calls the target for everything else, which is the same
//!   trait-seam move [`Level`](super::host::Level) makes.
//! - [`ConfigFiles`] — `exec` reads `cfg/*.cfg`, and reading it through a trait
//!   rather than importing [`Vfs`](crate::filesystem::Vfs) is what keeps the
//!   dependency out. `ENGINE_CONSOLE.md` §0.1 asks for a `std`-only module and
//!   §4.5 asks for a real `exec`; this is how both hold.
//!
//! # A command is not a callback
//!
//! `ConCommand` holds an `FnCommandCallback_t` — a bare function pointer that
//! reaches its state through globals. Neither half survives: there are no
//! globals here, and a closure capturing `&mut Engine` cannot be stored in a
//! registry that `Engine` owns. So commands are **declared as data**
//! ([`CommandSpec`]: name, help, flags, completion) and **executed by whoever
//! owns the state**.
//!
//! # Dispatch order
//!
//! `Cmd_ExecuteCommand` (`engine/cmd.cpp:929`) tries execution markers,
//! aliases, commands, cvars, then forwards to the server. Markers and
//! forwarding are deleted (§5), leaving:
//!
//! ```text
//! alias -> command -> cvar -> unknown
//! ```
//!
//! **Alias-before-command matters**: an alias shadows a command of the same
//! name, and an alias is *text substitution re-entering dispatch*, not a call,
//! so it can expand to further aliases.

// **Why this is still here after stage 4.** The first version of this comment
// predicted the allow could go once the console UI landed, on the grounds that
// the ring, the completion data and the input path would then have real
// consumers. They do — and the allow is still needed, for a reason that is not
// going away on its own:
//
// - `rustc`'s dead-code analysis does not count uses from `#[cfg(test)]`, and
//   a leaf module tested without an engine has a lot of those:
//   `Console::detached`, `NoTarget`, `NoConfigFiles`, most of
//   `CommandBuffer`'s accessors.
// - Several items exist *before* their consumer by design, which
//   `ENGINE_CONSOLE.md` argues for rather than apologises for: `Source`'s
//   remote variants are the security model §4.7 says to port before `net/`
//   needs it, `SPONLY` is a flag §4.6 keeps as a no-op predicate, and
//   `ExecContext`'s queueing half is what `exec` will hand to commands the
//   engine has not grown yet.
//
// **Trigger for removing it:** `net/` and `server/`, which take most of the
// second list. Not a stage of this module.
#![allow(dead_code)]

pub mod buffer;
pub mod cvar;
mod describe;
pub mod log;
pub mod token;
pub mod ui;

use std::collections::HashMap;

pub use buffer::CommandBuffer;
pub use cvar::{CommandFlags, Cvar, CvarFlags, CvarRegistry, RegisterError};
pub use log::{Color, Line, Log};
pub use token::{Command, Source};
pub use ui::ConsoleUi;

/// How far `exec` may nest before it is treated as a loop.
///
/// Valve has no such limit; a `.cfg` that execs itself recurses until the stack
/// is gone. The buffer's queue cap catches a runaway *alias*, but `exec`
/// recurses through Rust's stack rather than the queue, so it needs its own.
const MAX_EXEC_DEPTH: u32 = 16;

/// How many commands one [`Console::run`] may dispatch before the round is
/// treated as a runaway.
///
/// The buffer's own cap (`MAX_QUEUED_COMMANDS`) catches an alias that expands
/// to *many* commands. It cannot catch one that expands to **itself**: each
/// round removes one command and inserts one, so the queue never grows and the
/// loop runs forever at length one. Valve has the same hole; `alias x x; x`
/// hangs the shipped engine. This is the depth guard to the buffer's breadth
/// guard, and both are needed.
const MAX_COMMANDS_PER_ROUND: u32 = 10_000;

/// `COMMAND_COMPLETION_MAXITEMS` (`public/tier1/convar.h:154`). The cap is on
/// the list the UI is handed, not on what is searched.
const MAX_COMPLETION_ITEMS: usize = 64;

/// `exec` refuses anything larger, as Valve does: "probably not a valid file
/// to exec" (`engine/cmd.cpp:558`).
const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// How a command's arguments are completed. Data, not a callback — §4.10.
///
/// The completion *algorithm* is stage 4's, with the console UI. What is here
/// is the declaration, because it belongs on the spec the command registers and
/// retrofitting it later would touch every registration site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    None,
    /// Files under `dir` with extension `ext` — `exec` completing from
    /// `cfg/*.cfg`, `map` from `maps/*.bsp`.
    Files {
        dir: &'static str,
        ext: &'static str,
    },
    /// A fixed set, for the toggles.
    Values(&'static [&'static str]),
}

/// One entry in the completion list. `CompletionItem`
/// (`public/vgui_controls/consoledialog.h:66`), minus the widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What replaces the input line when this is chosen. For an argument
    /// completion it is the whole line — `exec config_default`, not
    /// `config_default` — which is what `AutoCompletionFunc` builds.
    pub text: String,
    /// A cvar's current value, shown beside the name. `None` for a command,
    /// which is also how the UI tells the two apart.
    pub value: Option<String>,
}

/// What a command *is*, as data.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub name: &'static str,
    pub help: &'static str,
    pub flags: CommandFlags,
    pub completion: Completion,
}

impl CommandSpec {
    /// A command with no flags and no argument completion.
    pub const fn new(name: &'static str, help: &'static str) -> CommandSpec {
        CommandSpec {
            name,
            help,
            flags: CommandFlags::NONE,
            completion: Completion::None,
        }
    }

    pub const fn with_flags(mut self, flags: CommandFlags) -> CommandSpec {
        self.flags = flags;
        self
    }

    pub const fn with_completion(mut self, completion: Completion) -> CommandSpec {
        self.completion = completion;
        self
    }
}

/// Whether the target recognised the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    Handled,
    /// Produces `Unknown command "%s"`. A target returning this for a name it
    /// registered is a bug in the target, not in the console.
    Unknown,
}

/// What a command may do to the console while it runs.
///
/// This exists for re-entrancy: a command that queues more text writes into
/// `cx`, which holds field borrows of the console the dispatcher is already
/// inside. Without it, `exec` could not compile — and neither could an engine
/// command that wants to print.
pub struct ExecContext<'a> {
    buffer: &'a mut CommandBuffer,
    log: &'a mut Log,
    /// Read-only, for `host_writeconfig`. `Host_WriteConfiguration`
    /// (`engine/host.cpp:1559`) is engine policy that pulls from *both*
    /// `keys.cpp` and `cvar.cpp`, so the composing command lives in the engine
    /// and reaches the cvars through here.
    cvars: &'a CvarRegistry,
    files: &'a dyn ConfigFiles,
    config_was_read: bool,
    source: Source,
}

impl ExecContext<'_> {
    /// `Cbuf_AddText` with the running command's own source, which is what
    /// keeps provenance from being laundered by passing through a command.
    pub fn enqueue(&mut self, text: &str) {
        self.buffer.add_text(text, self.source, 0);
    }

    /// [`enqueue`](ExecContext::enqueue), scheduled `ticks` rounds later.
    pub fn enqueue_delayed(&mut self, text: &str, ticks: i32) {
        self.buffer.add_text(text, self.source, ticks);
    }

    pub fn print(&mut self, text: &str) {
        self.log.print(text);
    }

    pub fn warn(&mut self, text: &str) {
        self.log.warn(text);
    }

    pub fn error(&mut self, text: &str) {
        self.log.error(text);
    }

    pub fn developer_print(&mut self, level: i32, text: &str) {
        self.log.developer_print(level, text);
    }

    /// The source of the command being executed. A target that gates on
    /// provenance reads it here.
    pub fn source(&self) -> Source {
        self.source
    }

    /// Every registered cvar, for a command that has to walk them.
    pub fn cvars(&self) -> &CvarRegistry {
        self.cvars
    }

    /// `Host_WasConfigCfgExecuted` (`engine/host.cpp:1446`).
    ///
    /// **Check this before writing a config.** It is set once the startup
    /// config exec has happened; writing before then would overwrite a real
    /// user's settings with defaults, which is what a crash during startup
    /// would otherwise cost them.
    pub fn config_was_read(&self) -> bool {
        self.config_was_read
    }

    /// Writes a config file under the write root.
    pub fn write_config(&mut self, path: &str, contents: &str) -> Result<(), String> {
        self.files.write_config(path, contents)
    }
}

/// Who runs the commands the console does not own.
///
/// Implemented on a struct of `&mut` borrows of the engine's fields rather than
/// on `Engine` itself — `ENGINE_CONSOLE.md` §6.6. `Console` is a field of
/// `Engine`, so `self.console.run(&mut self)` cannot compile; the fix is the
/// one [`Engine::frame`](super::Engine::frame) already uses for
/// `host.frame(&mut self.scene)`.
pub trait CommandTarget {
    fn execute(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) -> Dispatch;
}

/// A target that handles nothing. What the console is driven with in tests, and
/// before there is an engine to hand commands to.
pub struct NoTarget;

impl CommandTarget for NoTarget {
    fn execute(&mut self, _cmd: &Command, _cx: &mut ExecContext<'_>) -> Dispatch {
        Dispatch::Unknown
    }
}

/// Where `exec` reads from.
///
/// The seam that keeps this module off [`crate::filesystem`]. The path is
/// already assembled (`cfg/valve.rc`); `path_id` is `exec`'s optional second
/// argument, which Valve spells as the `//<pathid>/` prefix on the path and
/// defaults to `*`, meaning any mount.
pub trait ConfigFiles {
    fn read_config(&self, path: &str, path_id: Option<&str>) -> Option<Vec<u8>>;

    /// Whether the file is there, without reading it.
    ///
    /// Used for the `config.cfg`-or-`config_default.cfg` choice at startup
    /// (`engine/host.cpp:2058`), which has to happen *before* either is exec'd.
    fn config_exists(&self, path: &str, path_id: Option<&str>) -> bool {
        self.read_config(path, path_id).is_some()
    }

    /// Writes `contents` to `path` under the write root.
    ///
    /// Separate from reading because they are not symmetric: reads search
    /// every mount in order, and there is exactly one place a write can go.
    fn write_config(&self, path: &str, contents: &str) -> Result<(), String> {
        let _ = (path, contents);
        Err("no writable game directory is mounted".to_string())
    }

    /// Immediate children of `dir` ending in `.<ext>`, **with the extension
    /// stripped**, in any order.
    ///
    /// `CBaseAutoCompleteFileList::AutoCompletionFunc`
    /// (`engine/baseautocompletefilelist.cpp:23`), which is a
    /// `Sys_FindFirst`/`Sys_FindNext` walk of `<subdir>/*.<ext>` ending in
    /// `commands[i][strlen(commands[i]) - 4] = 0` — chopping four characters
    /// to remove the extension, which is why every one Valve completes happens
    /// to be three letters long. Stripping it properly is the same behavior
    /// for `cfg` and `bsp` and correct for anything else.
    ///
    /// Defaults to nothing, so a console with no content mounted completes
    /// commands and cvars and offers no filenames, rather than failing.
    fn list_files(&self, dir: &str, ext: &str) -> Vec<String> {
        let _ = (dir, ext);
        Vec::new()
    }
}

/// A [`ConfigFiles`] with nothing in it. `exec` then behaves as it does for a
/// file that is not there.
pub struct NoConfigFiles;

impl ConfigFiles for NoConfigFiles {
    fn read_config(&self, _path: &str, _path_id: Option<&str>) -> Option<Vec<u8>> {
        None
    }
}

/// The console.
///
/// The lifetime is the mounted game content's: [`ConfigFiles`] is usually
/// implemented over a borrowed [`Vfs`](crate::filesystem::Vfs), the same way
/// [`Engine`](super::Engine) and `Scene` borrow it. A console built with
/// [`NoConfigFiles`] is `Console<'static>`.
pub struct Console<'a> {
    cvars: CvarRegistry,
    commands: HashMap<Box<str>, CommandSpec>,
    aliases: HashMap<Box<str>, String>,
    buffer: CommandBuffer,
    log: Log,
    files: Box<dyn ConfigFiles + 'a>,
    /// The process arguments, for `stuffcmds` and for the `+<cvar>` seeding in
    /// [`Console::try_cvar`]. Data rather than a [`CommandLine`] borrow, so
    /// this module keeps depending on `std` alone.
    ///
    /// [`CommandLine`]: crate::cmdline::CommandLine
    command_line: Vec<String>,
    sv_cheats: Cvar,
    exec_depth: u32,
    /// `g_bConfigCfgExecuted` (`engine/host.cpp:1283`). See
    /// [`ExecContext::config_was_read`].
    config_was_read: bool,
    /// Commands dispatched in the current [`Console::run`]. See
    /// [`MAX_COMMANDS_PER_ROUND`].
    dispatched: u32,
    budget_exceeded: bool,
    /// `ENGINE_CONSOLE.md` §9 open question 6: shipped `.cfg` files name cvars
    /// from subsystems that do not exist yet, and a wall of errors at every
    /// launch is worse than a count.
    unknown_from_code: u32,
    /// Unknown names already reported, so each is printed once. See
    /// [`Console::report_unknown`].
    unknown_reported: std::collections::HashSet<Box<str>>,
}

impl<'a> Console<'a> {
    /// Builds a console with its own cvars registered.
    ///
    /// `command_line` is the process arguments including argv[0], matching
    /// `CCommandLine`'s indexing so that `stuffcmds` skips it the way Valve
    /// does.
    pub fn new(files: Box<dyn ConfigFiles + 'a>, command_line: Vec<String>) -> Console<'a> {
        let mut cvars = CvarRegistry::new();

        // Registered by hand rather than through `try_cvar`, because the log
        // these would print a failure to does not exist yet.
        let mut own = |name: &str, default: &str, help: &str| {
            let cvar = seed_from_command_line(
                Cvar::detached(name, default, CvarFlags::NONE, help),
                &command_line,
            );
            cvars.register(cvar).expect("console cvars are unique")
        };

        let developer = own("developer", "0", "Set the developer message level.");
        let filter_enable = own(
            "con_filter_enable",
            "0",
            "Console filter: 0 off, 1 show only matching, 2 dim non-matching.",
        );
        let filter_text = own("con_filter_text", "", "Console filter: text to require.");
        let filter_text_out = own(
            "con_filter_text_out",
            "",
            "Console filter: text to exclude.",
        );
        let sv_cheats = own("sv_cheats", "0", "Allow cheat commands and cvars.");

        let mut console: Console<'a> = Console {
            cvars,
            commands: HashMap::new(),
            aliases: HashMap::new(),
            buffer: CommandBuffer::new(),
            log: Log::new(developer, filter_enable, filter_text, filter_text_out),
            files,
            command_line,
            sv_cheats,
            exec_depth: 0,
            config_was_read: false,
            dispatched: 0,
            budget_exceeded: false,
            unknown_from_code: 0,
            unknown_reported: std::collections::HashSet::new(),
        };
        console.register_builtins();
        console
    }

    /// A console with no content mounted and no command line. For tests.
    pub fn detached() -> Console<'static> {
        let mut console = Console::new(Box::new(NoConfigFiles), Vec::new());
        console.log.set_echo_to_stderr(false);
        console
    }

    fn register_builtins(&mut self) {
        for spec in [
            CommandSpec::new("exec", "Execute a script file from cfg/.").with_completion(
                Completion::Files {
                    dir: "cfg",
                    ext: "cfg",
                },
            ),
            CommandSpec::new("execifexists", "Execute a script file if it exists.")
                .with_completion(Completion::Files {
                    dir: "cfg",
                    ext: "cfg",
                }),
            CommandSpec::new("alias", "Alias a name to a body of command text."),
            CommandSpec::new("echo", "Print its arguments to the console."),
            CommandSpec::new("clear", "Clear the console scrollback."),
            CommandSpec::new(
                "stuffcmds",
                "Run the `+`-prefixed arguments from the command line.",
            ),
            // `wait` never reaches dispatch -- `CommandBuffer::add_text` eats
            // it at insert time (§4.2). The spec is registered so that it is
            // discoverable and so that completion and `help` can see it.
            CommandSpec::new("wait", "Delay the rest of the command text by a tick."),
            // The list commands (`ENGINE_CONSOLE.md` §8 stage 5). Help text is
            // Valve's own, verbatim, because it is what `help` and `cvarlist`
            // print and a reader comparing the two should see the same words.
            CommandSpec::new("cvarlist", "Show the list of convars/concommands."),
            CommandSpec::new("help", "Find help about a convar/concommand."),
            CommandSpec::new(
                "find",
                "Find concommands with the specified string in their name/help text.",
            ),
            CommandSpec::new(
                "differences",
                "Show all convars which are not at their default values.",
            ),
            CommandSpec::new(
                "toggle",
                "Toggles a convar on or off, or cycles through a set of values.",
            ),
            CommandSpec::new("incrementvar", "Increment specified convar value."),
        ] {
            self.register_command(spec)
                .expect("built-in commands are unique");
        }
    }

    // ---- registration -----------------------------------------------------

    /// Registers a cvar and hands back the handle to keep.
    ///
    /// **Keep the [`Cvar`], not a way to look one up.** That is the whole of
    /// §6.1: reading it later is an atomic load through this handle, with no
    /// `&Console` in the reader's signature.
    ///
    /// The command line seeds the initial value: `+fps_max 60` sets it here,
    /// before `valve.rc` runs. That is `CCvar::GetCommandLineValue`
    /// (`vstdlib/cvar.cpp:699`) and is a *distinct path* from `stuffcmds`,
    /// which runs the same argument again as a command later.
    pub fn try_cvar(
        &mut self,
        name: &str,
        default: &str,
        flags: CvarFlags,
        help: &str,
    ) -> Result<Cvar, RegisterError> {
        let cvar = seed_from_command_line(
            Cvar::detached(name, default, flags, help),
            &self.command_line,
        );
        self.cvars.register(cvar)
    }

    /// [`try_cvar`](Console::try_cvar) with `ClampValue` bounds.
    pub fn try_cvar_bounded(
        &mut self,
        name: &str,
        default: &str,
        flags: CvarFlags,
        help: &str,
        min: Option<f32>,
        max: Option<f32>,
    ) -> Result<Cvar, RegisterError> {
        let cvar = seed_from_command_line(
            Cvar::detached_with_bounds(name, default, flags, help, min, max),
            &self.command_line,
        );
        self.cvars.register(cvar)
    }

    /// [`try_cvar`](Console::try_cvar), panicking on a duplicate.
    ///
    /// A duplicate name is a programming error rather than a runtime condition
    /// (§4.8), it is deterministic, and it happens during startup — so a panic
    /// names the bug at the moment it is introduced. Use
    /// [`try_cvar`](Console::try_cvar) where a caller can genuinely recover.
    pub fn cvar(&mut self, name: &str, default: &str, flags: CvarFlags, help: &str) -> Cvar {
        self.try_cvar(name, default, flags, help)
            .unwrap_or_else(|err| panic!("registering cvar `{name}`: {err}"))
    }

    /// [`try_cvar_bounded`](Console::try_cvar_bounded), panicking on a
    /// duplicate.
    pub fn cvar_bounded(
        &mut self,
        name: &str,
        default: &str,
        flags: CvarFlags,
        help: &str,
        min: Option<f32>,
        max: Option<f32>,
    ) -> Cvar {
        self.try_cvar_bounded(name, default, flags, help, min, max)
            .unwrap_or_else(|err| panic!("registering cvar `{name}`: {err}"))
    }

    /// Declares a command. The [`CommandTarget`] is what actually runs it.
    pub fn register_command(&mut self, spec: CommandSpec) -> Result<(), RegisterError> {
        let key = spec.name.to_ascii_lowercase();
        if key.is_empty() || key.split_whitespace().count() != 1 {
            return Err(RegisterError::BadName(spec.name.to_string()));
        }
        if self.commands.contains_key(key.as_str()) {
            return Err(RegisterError::Duplicate(spec.name.to_string()));
        }
        self.commands.insert(key.into_boxed_str(), spec);
        Ok(())
    }

    pub fn find_cvar(&self, name: &str) -> Option<&Cvar> {
        self.cvars.find(name)
    }

    pub fn find_command(&self, name: &str) -> Option<&CommandSpec> {
        self.commands.get(name.to_ascii_lowercase().as_str())
    }

    pub fn cvars(&self) -> &CvarRegistry {
        &self.cvars
    }

    pub fn commands(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values()
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn log_mut(&mut self) -> &mut Log {
        &mut self.log
    }

    pub fn buffer(&self) -> &CommandBuffer {
        &self.buffer
    }

    /// Whether `path` resolves, without reading it.
    pub fn config_exists(&self, path: &str, path_id: Option<&str>) -> bool {
        self.files.config_exists(path, path_id)
    }

    /// `Host_WasConfigCfgExecuted`.
    pub fn config_was_read(&self) -> bool {
        self.config_was_read
    }

    /// Writes a config file under the write root. See
    /// [`ExecContext::write_config`].
    pub fn write_config_file(&self, path: &str, contents: &str) -> Result<(), String> {
        self.files.write_config(path, contents)
    }

    /// `Host_SetConfigCfgExecuted` (`engine/host.cpp:1456`), which the original
    /// calls unconditionally once the startup exec block has run — *either*
    /// branch of it, so this means "startup finished reading configs", not
    /// "`config.cfg` specifically was found".
    pub fn set_config_was_read(&mut self, read: bool) {
        self.config_was_read = read;
    }

    /// How many `Source::Code` commands went unrecognised since the last call,
    /// resetting the count. See [`Console::unknown_from_code`].
    pub fn take_unknown_count(&mut self) -> u32 {
        std::mem::take(&mut self.unknown_from_code)
    }

    // ---- completion -------------------------------------------------------

    /// The completion list for a partially typed line.
    /// `CConsolePanel::RebuildCompletionList` (`consoledialog.cpp:510`).
    ///
    /// The rules, all of which are worth keeping even though the widget is
    /// not:
    ///
    /// - **Empty input returns nothing**, because empty input lists *history*
    ///   and history is the UI's — see
    ///   [`ConsoleUi`](super::console::ui::ConsoleUi).
    /// - **Input containing a space** first asks whether the command named by
    ///   the first token completes its own arguments ([`Completion`]). `exec `
    ///   listing `cfg/*.cfg` and `map ` listing `maps/*.bsp` are that path.
    /// - **Otherwise, prefix match** — and if there *was* a space and no
    ///   command claimed it, fall back to space-separated substring matching
    ///   ([`matches_text`]), so that `draw wire` finds `mat_wireframe`.
    /// - `DEVELOPMENTONLY` and `HIDDEN` are excluded from both lists.
    /// - Sorted by name, capped at [`MAX_COMPLETION_ITEMS`].
    ///
    /// Completion is **data on the [`CommandSpec`]**, not a callback
    /// (`ENGINE_CONSOLE.md` §4.10): `FnCommandCompletionCallback` reached the
    /// filesystem through a global, and there is none here.
    pub fn complete(&self, partial: &str) -> Vec<Suggestion> {
        if partial.is_empty() {
            return Vec::new();
        }

        // `FindAutoCompleteCommmandFromPartial` (`consoledialog.cpp:427`):
        // only once there is a space, and only for a command that claims its
        // arguments. Anything else falls through to the name search with
        // substring matching turned on.
        let mut substrings = false;
        if partial.contains(' ') {
            substrings = true;
            let name = partial.split(' ').next().unwrap_or_default();
            if let Some(spec) = self.find_command(name) {
                if spec.completion != Completion::None {
                    return self.complete_argument(spec.completion, partial);
                }
            }
        }

        let mut out: Vec<Suggestion> = Vec::new();

        for spec in self.commands.values() {
            if spec
                .flags
                .intersects(CommandFlags::DEVELOPMENTONLY | CommandFlags::HIDDEN)
            {
                continue;
            }
            if matches_text(spec.name, partial, substrings) {
                out.push(Suggestion {
                    text: spec.name.to_string(),
                    value: None,
                });
            }
        }

        for cvar in self.cvars.iter() {
            if cvar
                .flags()
                .intersects(CvarFlags::DEVELOPMENTONLY | CvarFlags::HIDDEN)
            {
                continue;
            }
            if matches_text(cvar.name(), partial, substrings) {
                out.push(Suggestion {
                    text: cvar.name().to_string(),
                    // `FCVAR_NEVER_AS_STRING` displays a formatted number
                    // instead of the string, which for those cvars is not a
                    // meaningful thing to show (`consoledialog.cpp:587`).
                    value: Some(match cvar.flags().contains(CvarFlags::NEVER_AS_STRING) {
                        true => format_number(cvar.float()),
                        false => cvar.string().to_string(),
                    }),
                });
            }
        }

        // A `HashMap` has no order, so the sort is what makes the list stable
        // between two identical keystrokes rather than merely tidy.
        out.sort_by(|a, b| {
            a.text
                .to_ascii_lowercase()
                .cmp(&b.text.to_ascii_lowercase())
        });
        out.truncate(MAX_COMPLETION_ITEMS);
        out
    }

    /// The argument half of [`complete`](Console::complete), for a command
    /// that declared how its arguments complete.
    ///
    /// `AutoCompletionFunc` builds `"<command> <candidate>"`, matching the
    /// candidate against everything after the *first* space — so `exec  con`,
    /// with two spaces, matches nothing, exactly as it does in the original.
    fn complete_argument(&self, completion: Completion, partial: &str) -> Vec<Suggestion> {
        let (name, arg) = match partial.split_once(' ') {
            Some(split) => split,
            None => (partial, ""),
        };

        let candidates = match completion {
            Completion::None => return Vec::new(),
            Completion::Files { dir, ext } => self.files.list_files(dir, ext),
            Completion::Values(values) => values.iter().map(|v| v.to_string()).collect(),
        };

        let mut out: Vec<Suggestion> = candidates
            .into_iter()
            .filter(|candidate| has_prefix(candidate, arg))
            .map(|candidate| Suggestion {
                text: format!("{name} {candidate}"),
                value: None,
            })
            .collect();

        out.sort_by(|a, b| {
            a.text
                .to_ascii_lowercase()
                .cmp(&b.text.to_ascii_lowercase())
        });
        // Two mounts can serve the same `cfg/*.cfg`, and `Vfs::list` merges
        // rather than deduplicating by name.
        out.dedup();
        out.truncate(MAX_COMPLETION_ITEMS);
        out
    }

    // ---- driving ----------------------------------------------------------

    /// `Cbuf_AddText`. Queues text to run on the next [`run`](Console::run).
    pub fn enqueue(&mut self, text: &str, source: Source) {
        self.buffer.add_text(text, source, 0);
    }

    /// `Cbuf_Execute`: drain everything due this round.
    ///
    /// One call is one tick, which is the spoof `engine/cmd.cpp:288` performs
    /// by passing 1 every time. It is what makes `wait 1` mean "next frame",
    /// and the shipped `.cfg` files assume it.
    pub fn run(&mut self, target: &mut dyn CommandTarget) {
        self.dispatched = 0;
        self.budget_exceeded = false;

        self.buffer.begin_processing(1);
        while let Some(cmd) = self.buffer.dequeue() {
            self.dispatch(&cmd, target);
        }
        self.buffer.end_processing();

        if self.budget_exceeded {
            // Whatever is still queued is part of the loop. Carrying it into
            // the next frame would hang the engine one round at a time.
            self.buffer.clear();
        }

        if self.buffer.take_overflow() {
            self.log.error(
                "command buffer overflow: commands were dropped \
                 (an alias or config that expands without end?)",
            );
        }
    }

    /// The dispatch order, made explicit because every future subsystem's
    /// commands inherit it.
    fn dispatch(&mut self, cmd: &Command, target: &mut dyn CommandTarget) {
        if cmd.is_empty() {
            return;
        }

        self.dispatched += 1;
        if self.dispatched > MAX_COMMANDS_PER_ROUND {
            if !self.budget_exceeded {
                self.budget_exceeded = true;
                self.log.error(&format!(
                    "runaway command text: more than {MAX_COMMANDS_PER_ROUND} commands in one \
                     round, stopping (an alias that expands to itself?)"
                ));
            }
            return;
        }

        self.log.developer_print(2, &format!("] {}", cmd.name()));

        // 1. Aliases, case-insensitively. A hit re-inserts the body at the head
        //    of the buffer and returns: an alias is text substitution, so it
        //    re-enters the whole of this function and can itself expand.
        if let Some(body) = self.aliases.get(cmd.name().to_ascii_lowercase().as_str()) {
            let body = body.clone();
            self.buffer.add_text(&body, cmd.source(), 0);
            return;
        }

        // 2. Commands.
        if let Some(spec) = self.find_command(cmd.name()) {
            let flags = spec.flags;
            if !self.permits(cmd, flags) {
                return;
            }
            if self.run_builtin(cmd, target) {
                return;
            }
            let Console {
                buffer,
                log,
                cvars,
                files,
                config_was_read,
                ..
            } = self;
            let mut cx = ExecContext {
                buffer,
                log,
                cvars,
                files: &**files,
                config_was_read: *config_was_read,
                source: cmd.source(),
            };
            if target.execute(cmd, &mut cx) == Dispatch::Unknown {
                self.report_unknown(cmd);
            }
            return;
        }

        // 3. Cvars.
        if self.set_or_print_cvar(cmd) {
            return;
        }

        // 4. Nothing matched. Valve would forward to the server here; there is
        //    none, so this is the end of the line.
        self.report_unknown(cmd);
    }

    /// `Unknown command "%s"` — counted always, printed **once per name**.
    ///
    /// Two things pull in opposite directions here and the split resolves both.
    ///
    /// §9 open question 6: `modsettings.cfg` and `config_default.cfg` are
    /// exec'd unconditionally and name commands from subsystems that do not
    /// exist yet, so a printed error per line is a wall at every launch. A
    /// *typed* command is different — silence there just looks broken. Hence
    /// the source split.
    ///
    /// The once-per-name rule is what makes that survive bindings.
    /// `config_default.cfg` binds `+attack` to MOUSE1 and `cancelselect` to
    /// Escape, and neither exists yet, so every click and every Escape would
    /// otherwise print. **Valve prints every time**, and can afford to: it
    /// implements all of its commands. A port where most of the game is
    /// missing cannot. The count is still incremented on every occurrence, so
    /// nothing is hidden from `take_unknown_count`.
    fn report_unknown(&mut self, cmd: &Command) {
        if cmd.source() == Source::Code {
            self.unknown_from_code += 1;
        }

        if !self.unknown_reported.insert(cmd.name().into()) {
            return;
        }

        let line = format!("Unknown command \"{}\"", cmd.name());
        match cmd.source() {
            Source::Code => self.log.developer_print(1, &line),
            _ => self.log.error(&line),
        }
    }

    /// The permission gauntlet `Cmd_ExecuteCommand` runs before a command.
    ///
    /// **The provenance half deliberately returns true for everything today**,
    /// and is here anyway — §9 open question 4. Only `Code` and `UserInput` can
    /// occur, both local and both trusted, so the flag-versus-source matrix
    /// cannot be written or tested until `net/` exists. Having the check as a
    /// function now means `net/` fills in a body instead of cutting a seam
    /// through a dispatcher that has been assuming trust.
    fn permits(&mut self, cmd: &Command, flags: CommandFlags) -> bool {
        if !cmd.source().is_trusted_local() {
            // Unreachable until `net/` or `demo/` lands. If it ever fires
            // before then, something has forged a source.
            self.log.error(&format!(
                "refusing `{}` from {:?}: remote sources are not implemented",
                cmd.name(),
                cmd.source()
            ));
            return false;
        }

        if flags.contains(CommandFlags::DEVELOPMENTONLY) {
            self.report_unknown(cmd);
            return false;
        }

        if flags.contains(CommandFlags::CHEAT) && !self.can_cheat() {
            self.log.error(&format!(
                "Can't use cheat command {} in multiplayer, unless the server has sv_cheats set to 1.",
                cmd.name()
            ));
            return false;
        }

        true
    }

    /// `CanCheat()` (`engine/gl_cvars.h:30`).
    pub fn can_cheat(&self) -> bool {
        self.sv_cheats.bool()
    }

    /// `CCvarUtilities::IsCommand` (`engine/cvar.cpp:366`) — the name is a
    /// historical lie; it means "was this a cvar, and if so get or set it".
    fn set_or_print_cvar(&mut self, cmd: &Command) -> bool {
        let Some(cvar) = self.cvars.find(cmd.name()) else {
            return false;
        };
        let cvar = cvar.clone();

        // Not checking HIDDEN here, so hidden cvars can still be set -- Valve's
        // comment at `cvar.cpp:390` makes the same point.
        if cvar.flags().contains(CvarFlags::DEVELOPMENTONLY) {
            return false;
        }

        if cmd.argc() == 1 {
            let line = describe::cvar(&cvar);
            self.log.print(&line);
            return true;
        }

        if cvar.flags().contains(CvarFlags::CHEAT) && !self.can_cheat() {
            self.log.error(&format!(
                "Can't use cheat cvar {} in multiplayer, unless the server has sv_cheats set to 1.",
                cvar.name()
            ));
            return true;
        }

        cvar.set_string(&strip_set_value(cmd.tail()));

        // `sv_cheats 0` reverts everything that was only settable because it
        // was 1 (`RevertFlaggedConVars`).
        if cvar.name().eq_ignore_ascii_case("sv_cheats") && !cvar.bool() {
            let reverted = self.cvars.revert_flagged(CvarFlags::CHEAT);
            if reverted > 0 {
                self.log
                    .developer_print(1, &format!("reverted {reverted} cheat cvars"));
            }
        }
        true
    }

    /// Runs one of the console's own commands. True if it was one.
    ///
    /// These are handled before the target is consulted because they need
    /// nothing else, which keeps the engine's `execute` down to the commands
    /// that are genuinely the engine's.
    fn run_builtin(&mut self, cmd: &Command, target: &mut dyn CommandTarget) -> bool {
        match cmd.name().to_ascii_lowercase().as_str() {
            "exec" => self.cmd_exec(cmd, target, false),
            // `Cmd_ExecIfExists_f` (`engine/cmd.cpp:798`), which is
            // `_Cmd_Exec_f` with `bOnlyIfExists`.
            "execifexists" => self.cmd_exec(cmd, target, true),
            "alias" => self.cmd_alias(cmd),
            "echo" => {
                let text = cmd.args().join(" ");
                self.log.echo(&text);
            }
            "clear" => self.log.clear(),
            "stuffcmds" => self.cmd_stuffcmds(cmd),
            "cvarlist" => self.cmd_cvarlist(cmd),
            "help" => self.cmd_help(cmd),
            "find" => self.cmd_find(cmd),
            "differences" => self.cmd_differences(),
            "toggle" => self.cmd_toggle(cmd),
            "incrementvar" => self.cmd_incrementvar(cmd),
            // Eaten by the buffer at insert time; reaching dispatch means
            // `wait` was disabled, in which case doing nothing is right.
            "wait" => {}
            _ => return false,
        }
        true
    }

    /// `_Cmd_Exec_f` (`engine/cmd.cpp:500`).
    ///
    /// **Line at a time, draining each line before reading the next.** That is
    /// not an implementation detail: it is why an `exec` inside a `.cfg`
    /// completes before the rest of the outer file runs, which `valve.rc`
    /// depends on. It is also why a syntax error on line 3 does not stop lines
    /// 1 and 2 from having run.
    fn cmd_exec(&mut self, cmd: &Command, target: &mut dyn CommandTarget, only_if_exists: bool) {
        if cmd.argc() < 2 {
            self.log
                .print("exec <filename> [path id]: execute a script file");
            return;
        }
        let name = cmd.arg(1).unwrap_or_default().to_string();
        let path_id = cmd.arg(2).map(str::to_string);

        // `Q_DefaultExtension( fileName, ".cfg" )` -- appended only when there
        // is not one already, which is what lets `valve.rc` be exec'd by name.
        let file = match has_extension(&name) {
            true => name.clone(),
            false => format!("{name}.cfg"),
        };

        // `IsValidFileExtension` (`engine/cmd.cpp:438`) is a **blocklist of
        // dangerous extensions**, not an allowlist of `.cfg`/`.rc`. Keep it as
        // a blocklist: it is a content-trust check on a path that shipped
        // content can influence, and an allowlist here would reject `valve.rc`.
        if !is_valid_config_extension(&file) {
            self.log.error(&format!("exec {file}: invalid file type."));
            return;
        }

        if self.exec_depth >= MAX_EXEC_DEPTH {
            self.log.error(&format!(
                "exec {file}: nested more than {MAX_EXEC_DEPTH} deep; \
                 refusing (a config that execs itself?)"
            ));
            return;
        }

        let path = format!("cfg/{file}");
        let Some(bytes) = self.files.read_config(&path, path_id.as_deref()) else {
            // `autoexec.cfg`, `joystick.cfg` and `game.cfg` fail **silently**
            // (`engine/cmd.cpp:572`). This looks like a hack and is exactly
            // right: Portal 2 ships none of them, and `valve.rc` execs two --
            // without the special case, every launch prints two errors.
            // `execifexists` asks for the same silence for any file.
            if !only_if_exists && !fails_silently(&name) {
                self.log.error(&format!("exec: couldn't exec {name}"));
            }
            return;
        };

        if bytes.len() > MAX_CONFIG_BYTES {
            self.log
                .error(&format!("exec {name}: file size larger than 1 MB!"));
            return;
        }

        // Shipped `.cfg` files are latin-1, not UTF-8 -- the same encoding
        // trap `CLAUDE.md` records for `legacy/`. Lossy rather than a hard
        // error: a stray high byte in a comment must not cost the whole file.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.log.developer_print(1, &format!("execing {name}"));

        self.exec_depth += 1;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // Everything this line produces runs before the next line is read.
            // Valve tracks that with the buffer's head handle; the equivalent
            // here is the queue depth, because an insert during processing goes
            // to the front and so the line's own commands are always the ones
            // in front of the baseline.
            let baseline = self.buffer.len();
            self.buffer.add_text(line, cmd.source(), 0);
            while self.buffer.len() > baseline {
                // `None` means the front is scheduled for a later tick -- a
                // `wait` inside the file. It stays queued for a later round,
                // which is exactly why §4.5 warns that `wait` inside an exec
                // does not do what a naive reading suggests.
                let Some(next) = self.buffer.dequeue() else {
                    break;
                };
                self.dispatch(&next, target);
            }
        }
        self.exec_depth -= 1;
    }

    /// `Cmd_Alias_f`.
    fn cmd_alias(&mut self, cmd: &Command) {
        if cmd.argc() == 1 {
            let mut names: Vec<&str> = self.aliases.keys().map(Box::as_ref).collect();
            names.sort_unstable();
            let listing: Vec<String> = names
                .iter()
                .map(|name| format!("{name} : {}", self.aliases[*name]))
                .collect();
            self.log
                .print(&format!("Current alias commands:\n{}", listing.join("\n")));
            return;
        }

        let name = cmd.arg(1).unwrap_or_default().to_ascii_lowercase();

        // **Divergence, deliberate.** Valve sets an *empty* body here, which
        // silently shadows any command of the same name with nothing -- a typo
        // at the console can disable `map` until you restart. Printing the
        // current definition is the useful reading of a one-argument `alias`
        // and cannot break anything.
        if cmd.argc() == 2 {
            match self.aliases.get(name.as_str()) {
                Some(body) => {
                    let line = format!("{name} : {body}");
                    self.log.print(&line);
                }
                None => {
                    let line = format!("`{name}` is not an alias");
                    self.log.print(&line);
                }
            }
            return;
        }

        let body = cmd.args()[1..].join(" ");
        self.aliases.insert(name.into_boxed_str(), body);
    }

    /// `stuffcmds` (`engine/cmd.cpp:357`): turn the `+`-prefixed process
    /// arguments into command text.
    ///
    /// This is what makes `+map sp_a1_intro1` work, and it runs from
    /// `valve.rc` rather than at startup — which is why the map loads *after*
    /// the configs have been read rather than before.
    fn cmd_stuffcmds(&mut self, cmd: &Command) {
        if cmd.argc() != 1 {
            self.log
                .print("stuffcmds : execute command line parameters");
            return;
        }

        let args = self.command_line.clone();
        let mut build = String::new();
        // argv[0] is the executable name.
        let mut i = 1;
        while i < args.len() {
            let parm = args[i].as_str();

            if let Some(name) = parm.strip_prefix('+') {
                match value_of(&args, i) {
                    Some(value) => {
                        // `+map <name> <second>` is passed through with both
                        // arguments; `map` takes a reslist as its second.
                        let second = args
                            .get(i + 2)
                            .map(String::as_str)
                            .filter(|s| !s.starts_with('+') && !s.starts_with('-'));
                        match (parm.eq_ignore_ascii_case("+map"), second) {
                            (true, Some(second)) => {
                                build.push_str(&format!("{name} {value} {second}\n"));
                                i += 3;
                            }
                            _ => {
                                build.push_str(&format!("{name} {value}\n"));
                                i += 2;
                            }
                        }
                    }
                    None => {
                        build.push_str(name);
                        build.push('\n');
                        i += 1;
                    }
                }
                continue;
            }

            // `-XXX` options are skipped along with their value, if they have
            // one. A following `+` or `-` token is another option, not a value.
            if parm.starts_with('-') {
                i += if value_of(&args, i).is_some() { 2 } else { 1 };
                continue;
            }

            // Valve translates a bare `.dem`/`.bsp`/`.sav` argument into
            // `playdemo`/`map`/`load`. Deferred with demos and saves; a bare
            // argument is ignored rather than guessed at.
            i += 1;
        }

        if !build.is_empty() {
            self.buffer.add_text(&build, cmd.source(), 0);
        }
    }

    // ---- the list commands ------------------------------------------------
    //
    // `ENGINE_CONSOLE.md` §8 stage 5. All six are console built-ins rather
    // than the target's, for the same reason `exec` is: they need the registry
    // and the log and nothing else. `incrementvar` is the one that also needs
    // the buffer, and it is here too rather than in the engine, because what
    // it wants the buffer for is to re-enter dispatch -- which is `exec`'s
    // problem exactly.

    /// Every cvar and command a listing may show.
    ///
    /// The `DEVELOPMENTONLY`/`HIDDEN` filter lives here rather than at each
    /// call site because every one of the list commands applies it
    /// (`engine/cvar.cpp:1011`, `:1146`, `vstdlib/cvar.cpp:1077`) and it is the
    /// one filter that must not be forgotten: a listing is precisely what those
    /// two flags exist to hide from.
    ///
    /// `help` is deliberately **not** a caller. Valve's finds a hidden cvar by
    /// name and describes it, and that is the point of `HIDDEN` as against
    /// `DEVELOPMENTONLY`: not discoverable, still usable.
    fn listable(&self) -> Vec<Entry<'_>> {
        let commands = self
            .commands
            .values()
            .filter(|spec| {
                !spec
                    .flags
                    .intersects(CommandFlags::DEVELOPMENTONLY | CommandFlags::HIDDEN)
            })
            .map(Entry::Command);

        let cvars = self
            .cvars
            .iter()
            .filter(|cvar| {
                !cvar
                    .flags()
                    .intersects(CvarFlags::DEVELOPMENTONLY | CvarFlags::HIDDEN)
            })
            .map(Entry::Cvar);

        commands.chain(cvars).collect()
    }

    /// `CCvarUtilities::CvarList` (`engine/cvar.cpp:952`) —
    /// `cvarlist [log <file>] [partial]`.
    fn cmd_cvarlist(&mut self, cmd: &Command) {
        if cmd.argc() == 2 && cmd.arg(1).is_some_and(|arg| arg.eq_ignore_ascii_case("?")) {
            self.log.print("cvarlist:  [log logfile] [ partial ]");
            return;
        }

        // Valve reads `args[1]` unconditionally -- `CCommand::operator[]`
        // returns `""` past the end -- so a bare `cvarlist` takes the second
        // branch with an empty prefix, and an empty prefix matches everything.
        let logging = cmd.argc() >= 3
            && cmd
                .arg(1)
                .is_some_and(|arg| arg.eq_ignore_ascii_case("log"));
        let log_file = logging.then(|| cmd.arg(2).unwrap_or_default().to_string());
        let partial = match logging {
            true => cmd.arg(3).unwrap_or_default(),
            false => cmd.arg(1).unwrap_or_default(),
        };

        let mut matched: Vec<((String, String), String, String)> = self
            .listable()
            .into_iter()
            .filter(|entry| has_prefix(entry.name(), partial))
            .map(|entry| (describe::list_order(entry.name()), entry.row(), entry.csv()))
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0));

        // Written before anything is printed, so that a file that will not
        // open aborts the command rather than half-running it -- which is the
        // order `CvarList` opens its handle in.
        if let Some(file) = log_file {
            // **Divergence, deliberate.** Valve writes wherever it is told.
            // The path here comes from the same places `exec`'s does -- a
            // shipped `.cfg` as readily as a person -- so it gets the same
            // blocklist (§4.5), and `Vfs::write_path` already confines it to
            // the write root. Nothing legitimately logs cvars to a `.dll`.
            if !is_valid_config_extension(&file) {
                let line = format!("cvarlist log {file}: invalid file type.");
                self.log.error(&line);
                return;
            }

            let mut csv = describe::csv_header();
            csv.push('\n');
            for (_, _, row) in &matched {
                csv.push_str(row);
                csv.push('\n');
            }
            if let Err(err) = self.files.write_config(&file, &csv) {
                let line = format!("Couldn't open '{file}' for writing! ({err})");
                self.log.error(&line);
                return;
            }
        }

        self.log.print("cvar list\n--------------");
        for (_, row, _) in &matched {
            self.log.print(row);
        }

        let count = matched.len();
        let footer = match partial.is_empty() {
            true => format!("--------------\n{count:3} total convars/concommands"),
            false => format!("--------------\n{count:3} convars/concommands for [{partial}]"),
        };
        self.log.print(&footer);
    }

    /// `CCvarUtilities::CvarHelp` (`engine/cvar.cpp:1109`).
    fn cmd_help(&mut self, cmd: &Command) {
        if cmd.argc() != 2 {
            self.log.print("Usage:  help <cvarname>");
            return;
        }
        let name = cmd.arg(1).unwrap_or_default();

        // **Command before cvar**, which is dispatch's own order: `help x`
        // describes what typing `x` would actually do. Valve never has to
        // choose, because `FindCommandBase` searches one table holding both and
        // a name can only be one thing; here they are two maps, and nothing
        // stops a name being in both.
        let found = self
            .find_command(name)
            .map(describe::command)
            .or_else(|| self.cvars.find(name).map(describe::cvar));

        let line = match found {
            Some(line) => line,
            None => format!("help:  no cvar or command named {name}"),
        };
        self.log.print(&line);
    }

    /// `CCvar::Find` (`vstdlib/cvar.cpp:1052`) — substring search over names
    /// and help text.
    ///
    /// **Every search string must match**, each of them against *either* the
    /// name or the help text. Valve's original takes one string; the reference
    /// tree takes two (a Kisak addition, marked `lwss:` at the call site) while
    /// printing a usage line promising `[<string>...]`. Taking as many as are
    /// given is less code than either and is what the usage line already says.
    fn cmd_find(&mut self, cmd: &Command) {
        if cmd.argc() < 2 {
            self.log.print("Usage:  find <string> [<string>...]");
            return;
        }

        let needles: Vec<String> = cmd
            .args()
            .iter()
            .map(|arg| arg.to_ascii_lowercase())
            .collect();

        let mut matched: Vec<(String, String)> = self
            .listable()
            .into_iter()
            .filter(|entry| {
                let name = entry.name().to_ascii_lowercase();
                let help = entry.help().to_ascii_lowercase();
                needles
                    .iter()
                    .all(|needle| name.contains(needle.as_str()) || help.contains(needle.as_str()))
            })
            .map(|entry| (describe::name_order(entry.name()), entry.describe()))
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0));

        for (_, line) in &matched {
            self.log.print(line);
        }
    }

    /// `CCvarUtilities::CvarDifferences` (`engine/cvar.cpp:1139`): every cvar
    /// not sitting on the value it was declared with.
    ///
    /// **Sorted**, where Valve walks its hash table in whatever order it is
    /// in. A `HashMap` here is seeded per process, so unsorted would mean a
    /// different order on every launch.
    fn cmd_differences(&mut self) {
        let mut matched: Vec<(String, String)> = self
            .cvars
            .iter()
            .filter(|cvar| {
                !cvar
                    .flags()
                    .intersects(CvarFlags::DEVELOPMENTONLY | CvarFlags::HIDDEN)
            })
            .filter(|cvar| !describe::is_at_default(cvar))
            .map(|cvar| (describe::name_order(cvar.name()), describe::cvar(cvar)))
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0));

        for (_, line) in &matched {
            self.log.print(line);
        }
    }

    /// `CCvarUtilities::CvarToggle` (`engine/cvar.cpp:1161`) with
    /// `IsValidToggleCommand` (`:517`) inlined.
    fn cmd_toggle(&mut self, cmd: &Command) {
        if cmd.argc() < 2 {
            self.log
                .print("Usage:  toggle <cvarname> [value1] [value2] [value3]...");
            return;
        }
        let name = cmd.arg(1).unwrap_or_default();

        let Some(cvar) = self.cvars.find(name).cloned() else {
            let line = format!("{name} is not a valid cvar");
            self.log.print(&line);
            return;
        };

        // Refused **silently**, as `IsValidToggleCommand` does: these are the
        // two flags a listing hides, and a message here would be the one place
        // they surfaced.
        if cvar
            .flags()
            .intersects(CvarFlags::DEVELOPMENTONLY | CvarFlags::HIDDEN)
        {
            return;
        }

        if cvar.flags().contains(CvarFlags::CHEAT) && !self.can_cheat() {
            let line = format!(
                "Can't use cheat cvar {} in multiplayer, unless the server has sv_cheats set to 1.",
                cvar.name()
            );
            self.log.error(&line);
            return;
        }

        // `IsValidToggleCommand`'s other three refusals -- `SPONLY`,
        // `NOT_CONNECTED` and `REPLICATED` -- all ask whether a *server* is
        // connected. There is none, so they return with `net/`.

        let values = cmd.args().get(1..).unwrap_or_default();
        match values.is_empty() {
            true => cvar.set_bool(!cvar.bool()),
            false => {
                // Case-sensitive (`Q_strcmp`). The search is for the *current*
                // value, so a cvar sitting on something not in the list starts
                // the cycle at the list's beginning rather than staying put.
                let current = describe::value(&cvar);
                let next = match values.iter().position(|value| *value == current) {
                    Some(index) if index + 1 < values.len() => index + 1,
                    _ => 0,
                };
                cvar.set_string(&values[next]);
            }
        }

        let line = describe::cvar(&cvar);
        self.log.print(&line);
    }

    /// `incrementvar` (`engine/host_cmd.cpp:2638`):
    /// `incrementvar <cvar> <min> <max> <delta>`, wrapping at either end.
    fn cmd_incrementvar(&mut self, cmd: &Command) {
        if cmd.argc() != 5 {
            self.log
                .warn("Usage: incrementvar varName minValue maxValue delta");
            return;
        }
        let name = cmd.arg(1).unwrap_or_default();

        let Some(cvar) = self.cvars.find(name).cloned() else {
            let line = format!("cvar \"{name}\" not found");
            self.log.developer_print(1, &line);
            return;
        };

        // `atof`, not `str::parse`: the arguments come from a `.cfg` or from a
        // binding as often as from a person -- see [`self::cvar::atod`].
        let number = |index: usize| self::cvar::atod(cmd.arg(index).unwrap_or_default()) as f32;
        let (start, end, delta) = (number(2), number(3), number(4));

        let mut value = cvar.float() + delta;
        if value > end {
            value = start;
        } else if value < start {
            value = end;
        }

        // **Queued as a plain set rather than written here**, which Valve
        // explains as avoiding "any problems with state in a demo loop": what a
        // recording then contains is the set, not the increment, so replaying
        // it does not depend on the value the cvar happened to hold. Kept, and
        // it costs nothing -- an insert during processing goes to the head, so
        // the set still runs before anything already queued.
        let text = format!("{} {value:.6}", cvar.name());
        self.buffer.add_text(&text, cmd.source(), 0);

        let line = format!("{} = {value:.6}", cvar.name());
        self.log.developer_print(1, &line);
    }
}

/// One row of a listing: the two things a console name can be.
///
/// `ConCommandBase` with `IsCommand()`, which is the C++ spelling of a sum
/// type. The list commands are the only place the distinction is visible,
/// because they are the only place both are shown at once.
enum Entry<'a> {
    Cvar(&'a Cvar),
    Command(&'a CommandSpec),
}

impl Entry<'_> {
    fn name(&self) -> &str {
        match self {
            Entry::Cvar(cvar) => cvar.name(),
            Entry::Command(spec) => spec.name,
        }
    }

    fn help(&self) -> &str {
        match self {
            Entry::Cvar(cvar) => cvar.help(),
            Entry::Command(spec) => spec.help,
        }
    }

    /// `ConVar_PrintDescription`, for `help`, `find` and `toggle`.
    fn describe(&self) -> String {
        match self {
            Entry::Cvar(cvar) => describe::cvar(cvar),
            Entry::Command(spec) => describe::command(spec),
        }
    }

    /// `PrintCvar`/`PrintCommand`, for `cvarlist`'s console output.
    fn row(&self) -> String {
        match self {
            Entry::Cvar(cvar) => describe::cvar_row(cvar),
            Entry::Command(spec) => describe::command_row(spec),
        }
    }

    /// The same row for `cvarlist log`'s file.
    fn csv(&self) -> String {
        match self {
            Entry::Cvar(cvar) => describe::cvar_csv(cvar),
            Entry::Command(spec) => describe::command_csv(spec),
        }
    }
}

/// `CConsolePanel::CommandMatchesText` (`consoledialog.cpp:451`).
///
/// Two modes, and the second one is the non-obvious pleasant behavior worth
/// keeping: with `check_substrings`, `text` is split on spaces and **every
/// piece must appear somewhere in `command`**, so `"draw wire"` matches
/// `mat_wireframe` and `"vsync mat"` matches `mat_vsync`. Without it, it is a
/// plain case-insensitive prefix test.
fn matches_text(command: &str, text: &str, check_substrings: bool) -> bool {
    if !check_substrings {
        return has_prefix(command, text);
    }

    text.split(' ')
        .filter(|piece| !piece.is_empty())
        .all(|piece| {
            let piece = piece.to_ascii_lowercase();
            command.to_ascii_lowercase().contains(&piece)
        })
}

/// Case-insensitive prefix test over bytes, which is what `Q_strnicmp` is.
///
/// Byte-wise rather than `char`-wise on purpose: cvar and command names are
/// ASCII by construction, and slicing a `&str` at a byte offset taken from
/// another string is a panic waiting for the first multi-byte character a user
/// types.
fn has_prefix(name: &str, prefix: &str) -> bool {
    let (name, prefix) = (name.as_bytes(), prefix.as_bytes());
    name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `consoledialog.cpp:594`: an integral value prints as an integer, anything
/// else as a float. Otherwise every `FCVAR_NEVER_AS_STRING` cvar in the
/// completion list reads `1.000000`.
fn format_number(value: f32) -> String {
    match value == value.trunc() && value.is_finite() {
        true => format!("{}", value as i64),
        false => value.to_string(),
    }
}

/// `CCvarUtilities::WriteVariables` (`engine/cvar.cpp:637`): every
/// `FCVAR_ARCHIVE` cvar as `<name> "<value>"`, **sorted case-insensitively by
/// name** (`CVarSortFunc`, `:629`).
///
/// A free function rather than a method, because both callers need it and they
/// hold different things: the `host_writeconfig` command reaches the registry
/// through an [`ExecContext`], and the shutdown path has the [`Console`].
///
/// The sort is not cosmetic. `config.cfg` is rewritten on every clean exit, so
/// an unstable order would make the file churn against version control and
/// against any diff a user takes of it.
///
/// The value is quoted, which is what makes `strip_set_value` the reader: a
/// cvar holding spaces survives the round trip.
pub fn write_archived_cvars(cvars: &CvarRegistry, out: &mut String) {
    let mut archived: Vec<&Cvar> = cvars
        .iter()
        .filter(|cvar| cvar.flags().contains(CvarFlags::ARCHIVE))
        .collect();
    archived.sort_by_key(|cvar| cvar.name().to_ascii_lowercase());

    for cvar in archived {
        out.push_str(&format!("{} \"{}\"\n", cvar.name(), cvar.string()));
    }
}

/// `CCommandLine::ParmValue`: the next token, unless it is another option.
fn value_of(args: &[String], index: usize) -> Option<&str> {
    args.get(index + 1)
        .map(String::as_str)
        .filter(|value| !value.starts_with('-') && !value.starts_with('+'))
}

/// `CCvar::GetCommandLineValue` (`vstdlib/cvar.cpp:699`): `+<name> <value>` on
/// the command line replaces a cvar's declared default.
///
/// Distinct from `stuffcmds`, which runs the same argument as a *command*
/// later. This path is why `+fps_max 60` is in effect before `valve.rc` runs.
fn seed_from_command_line(cvar: Cvar, command_line: &[String]) -> Cvar {
    let wanted = format!("+{}", cvar.name());
    let found = command_line
        .iter()
        .position(|arg| arg.eq_ignore_ascii_case(&wanted))
        .and_then(|index| value_of(command_line, index));
    if let Some(value) = found {
        cvar.set_string(value);
    }
    cvar
}

/// The value half of a cvar set, taken from [`Command::tail`].
///
/// `CCvarUtilities::IsCommand` (`engine/cvar.cpp:481-514`): drop a leading
/// quote, strip trailing whitespace, then drop a trailing quote — **in that
/// order**. Doing it as "trim, then unquote" would give the same answer for
/// `"a b"` and the wrong one for `"  a b  "`, which is the case the ordering
/// exists for: the interior spaces are part of the value.
pub fn strip_set_value(tail: &str) -> String {
    let quoted = tail.starts_with('"');
    let body = match quoted {
        true => &tail[1..],
        false => tail,
    };

    let trimmed = body.trim_end_matches(|c: char| c <= ' ');
    match quoted {
        true => trimmed.strip_suffix('"').unwrap_or(trimmed).to_string(),
        false => trimmed.to_string(),
    }
}

/// Whether the filename already carries an extension. `Q_DefaultExtension`.
fn has_extension(name: &str) -> bool {
    let tail = name.rsplit('/').next().unwrap_or(name);
    tail.contains('.')
}

/// `IsValidFileExtension` (`engine/cmd.cpp:438`).
///
/// Matched case-insensitively where Valve's `Q_strstr` is case-sensitive.
/// Deliberate: this is a trust check, and `FOO.EXE` should not pass one.
fn is_valid_config_extension(name: &str) -> bool {
    const BLOCKED: [&str; 9] = [
        ".exe", ".vbs", ".com", ".bat", ".dll", ".ini", ".gcf", ".sys", ".blob",
    ];
    let lowered = name.to_ascii_lowercase();
    !BLOCKED.iter().any(|blocked| lowered.contains(blocked))
}

/// The three configs whose absence is normal (`engine/cmd.cpp:572`).
fn fails_silently(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    ["autoexec.cfg", "joystick.cfg", "game.cfg"]
        .iter()
        .any(|quiet| lowered.contains(quiet))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    /// A [`ConfigFiles`] backed by a map, which is what lets `exec` be tested
    /// without a mounted filesystem.
    struct FakeConfigs(Map<String, String>);

    impl FakeConfigs {
        fn new(files: &[(&str, &str)]) -> Box<FakeConfigs> {
            Box::new(FakeConfigs(
                files
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            ))
        }
    }

    impl ConfigFiles for FakeConfigs {
        fn read_config(&self, path: &str, _path_id: Option<&str>) -> Option<Vec<u8>> {
            self.0.get(path).map(|text| text.as_bytes().to_vec())
        }

        /// Every file the map holds under `<dir>/`, so that `exec `
        /// completion can be tested with the same fixture `exec` itself uses.
        fn list_files(&self, dir: &str, ext: &str) -> Vec<String> {
            let prefix = format!("{dir}/");
            let suffix = format!(".{ext}");
            self.0
                .keys()
                .filter_map(|path| path.strip_prefix(&prefix))
                .filter_map(|name| name.strip_suffix(&suffix))
                .map(str::to_string)
                .collect()
        }
    }

    /// Records what it was asked to run, in order.
    #[derive(Default)]
    struct Recorder {
        seen: Vec<String>,
    }

    impl CommandTarget for Recorder {
        fn execute(&mut self, cmd: &Command, _cx: &mut ExecContext<'_>) -> Dispatch {
            self.seen.push(
                std::iter::once(cmd.name().to_string())
                    .chain(cmd.args().iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            Dispatch::Handled
        }
    }

    fn console_with(files: Box<dyn ConfigFiles>, args: &[&str]) -> Console<'static> {
        let mut console = Console::new(files, args.iter().map(|a| (*a).to_string()).collect());
        console.log.set_echo_to_stderr(false);
        console
    }

    fn recorder(console: &mut Console) -> Vec<String> {
        let mut target = Recorder::default();
        console.run(&mut target);
        target.seen
    }

    fn declare(console: &mut Console, name: &'static str) {
        console
            .register_command(CommandSpec::new(name, ""))
            .expect("unique");
    }

    // ---- registration -----------------------------------------------------

    #[test]
    fn a_duplicate_registration_is_refused() {
        let mut console = Console::detached();
        assert!(console
            .try_cvar("fps_max", "300", CvarFlags::NONE, "")
            .is_ok());
        assert_eq!(
            console
                .try_cvar("fps_max", "60", CvarFlags::NONE, "")
                .expect_err("a duplicate must be refused"),
            RegisterError::Duplicate("fps_max".into()),
            "one binary, one declaration -- the parent/child linkage is gone"
        );

        declare(&mut console, "map");
        assert_eq!(
            console.register_command(CommandSpec::new("map", "")),
            Err(RegisterError::Duplicate("map".into()))
        );
    }

    #[test]
    fn a_name_that_could_never_be_typed_is_refused() {
        let mut console = Console::detached();
        assert!(matches!(
            console.try_cvar("two words", "0", CvarFlags::NONE, ""),
            Err(RegisterError::BadName(_))
        ));
        assert!(matches!(
            console.try_cvar("", "0", CvarFlags::NONE, ""),
            Err(RegisterError::BadName(_))
        ));
        assert!(matches!(
            console.register_command(CommandSpec::new("two words", "")),
            Err(RegisterError::BadName(_))
        ));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut console = Console::detached();
        console.cvar("fps_max", "300", CvarFlags::NONE, "");
        assert!(console.find_cvar("FPS_MAX").is_some());
        assert!(console.find_command("EXEC").is_some());
    }

    // ---- cvars ------------------------------------------------------------

    #[test]
    fn setting_a_cvar_needs_nothing_but_the_cvar() {
        let mut console = Console::detached();
        let fps_max = console.cvar("fps_max", "300", CvarFlags::NONE, "");

        console.enqueue("fps_max 60", Source::UserInput);
        // No target is consulted: `NoTarget` handles nothing, and the set still
        // happens. That is the §0.3 split.
        console.run(&mut NoTarget);

        assert_eq!(fps_max.float(), 60.0);
        assert_eq!(&*fps_max.string(), "60");
    }

    #[test]
    fn the_set_path_strips_surrounding_quotes_but_keeps_interior_spaces() {
        // `engine/cvar.cpp:481-514`: unquote, then trim, then unquote again --
        // in that order.
        assert_eq!(strip_set_value(r#""  a b  ""#), "  a b  ");
        assert_eq!(strip_set_value("plain"), "plain");
        assert_eq!(strip_set_value("trailing   "), "trailing");
        assert_eq!(strip_set_value(r#""quoted""#), "quoted");
        assert_eq!(strip_set_value(r#""unterminated"#), "unterminated");

        let mut console = Console::detached();
        let hostname = console.cvar("hostname", "", CvarFlags::NONE, "");
        console.enqueue(r#"hostname "  a b  ""#, Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(&*hostname.string(), "  a b  ");
    }

    #[test]
    fn bounds_clamp_on_every_set_including_the_default() {
        let mut console = Console::detached();
        let fps_max = console.cvar_bounded(
            "fps_max",
            "300",
            CvarFlags::NONE,
            "",
            Some(0.0),
            Some(1000.0),
        );

        console.enqueue("fps_max -1", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(fps_max.float(), 0.0);
        assert_eq!(
            &*fps_max.string(),
            "0.000000",
            "a clamped set stores the reformatted number, not the text typed"
        );

        console.enqueue("fps_max 99999", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(fps_max.float(), 1000.0);

        // The declared default is clamped too, rather than being the one value
        // that escapes its own bounds.
        let clamped_default =
            Cvar::detached_with_bounds("x", "-5", CvarFlags::NONE, "", Some(0.0), None);
        assert_eq!(clamped_default.float(), 0.0);
    }

    #[test]
    fn the_generation_counter_reports_changes() {
        let cvar = Cvar::detached("fps_max", "300", CvarFlags::NONE, "");
        let mut seen = cvar.generation();
        assert!(!cvar.changed(&mut seen));

        cvar.set_string("60");
        assert!(cvar.changed(&mut seen));
        assert!(!cvar.changed(&mut seen), "and only once per change");

        // A set that changes nothing does not wake a poller.
        cvar.set_float(60.0);
        assert!(!cvar.changed(&mut seen));
    }

    #[test]
    fn a_bare_cvar_name_prints_its_description() {
        let mut console = Console::detached();
        console.cvar("fps_max", "300", CvarFlags::NONE, "Frame rate limiter.");
        console.enqueue("fps_max", Source::UserInput);
        console.run(&mut NoTarget);

        let text: Vec<&str> = console.log().lines().map(|l| l.text.as_str()).collect();
        assert!(
            text.iter().any(|l| l.contains("\"fps_max\" = \"300\"")),
            "got {text:?}"
        );
    }

    #[test]
    fn cheat_cvars_need_sv_cheats_and_revert_when_it_goes_off() {
        let mut console = Console::detached();
        let god = console.cvar("sv_infinite_ammo", "0", CvarFlags::CHEAT, "");

        console.enqueue("sv_infinite_ammo 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(!god.bool(), "refused while sv_cheats is 0");

        console.enqueue("sv_cheats 1; sv_infinite_ammo 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(god.bool());

        console.enqueue("sv_cheats 0", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(!god.bool(), "RevertFlaggedConVars(FCVAR_CHEAT)");
    }

    #[test]
    fn a_development_only_cvar_reads_as_unknown() {
        let mut console = Console::detached();
        let hidden = console.cvar("secret", "0", CvarFlags::DEVELOPMENTONLY, "");
        console.enqueue("secret 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(!hidden.bool());
    }

    // ---- dispatch order ---------------------------------------------------

    #[test]
    fn a_registered_command_reaches_the_target() {
        let mut console = Console::detached();
        declare(&mut console, "map");
        console.enqueue("map sp_a1_intro1", Source::Code);
        assert_eq!(recorder(&mut console), ["map sp_a1_intro1"]);
    }

    #[test]
    fn an_unregistered_name_never_reaches_the_target() {
        let mut console = Console::detached();
        console.enqueue("nonsense", Source::UserInput);
        assert!(recorder(&mut console).is_empty());

        let text: Vec<String> = console.log().lines().map(|l| l.text.clone()).collect();
        assert!(
            text.iter().any(|l| l.contains("Unknown command")),
            "{text:?}"
        );
    }

    /// §9 open question 6: a shipped `.cfg` naming cvars from unported
    /// subsystems must not print a wall of errors at every launch.
    #[test]
    fn unknown_names_from_a_config_are_counted_quietly_and_typed_ones_are_not() {
        let mut console = Console::detached();
        console.enqueue("not_a_thing 1; also_missing 2", Source::Code);
        console.run(&mut NoTarget);
        assert!(console.log().is_empty(), "quiet at developer 0");
        assert_eq!(console.take_unknown_count(), 2);
        assert_eq!(console.take_unknown_count(), 0, "and the count resets");

        console.enqueue("still_not_a_thing 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(console.log().len(), 1, "a typed mistake is always visible");
    }

    /// `config_default.cfg` binds `+attack` and `cancelselect`, neither of
    /// which exists yet, so without this every click and every Escape prints.
    #[test]
    fn an_unknown_name_is_printed_once_but_counted_every_time() {
        let mut console = Console::detached();
        for _ in 0..5 {
            console.enqueue("+attack 3", Source::UserInput);
            console.run(&mut NoTarget);
        }
        assert_eq!(console.log().len(), 1, "one line, not five");

        console.enqueue("+attack 3", Source::Code);
        console.run(&mut NoTarget);
        assert_eq!(console.take_unknown_count(), 1, "still counted");
    }

    #[test]
    fn an_alias_shadows_a_command_of_the_same_name() {
        let mut console = Console::detached();
        declare(&mut console, "map");
        console.enqueue("alias map echo shadowed", Source::UserInput);
        console.run(&mut NoTarget);

        console.enqueue("map sp_a1_intro1", Source::UserInput);
        assert!(
            recorder(&mut console).is_empty(),
            "the alias wins, so the target never sees `map`"
        );
    }

    #[test]
    fn an_alias_is_text_substitution_and_re_enters_dispatch() {
        let mut console = Console::detached();
        declare(&mut console, "one");
        declare(&mut console, "two");
        console.enqueue("alias both one; two", Source::UserInput);
        console.run(&mut NoTarget);

        // An alias can expand to another alias, because the body goes back
        // through the whole of `dispatch`.
        console.enqueue("alias outer both", Source::UserInput);
        console.run(&mut NoTarget);

        console.enqueue("outer", Source::UserInput);
        assert_eq!(recorder(&mut console), ["one"]);
    }

    #[test]
    fn an_alias_body_runs_before_what_was_already_queued() {
        let mut console = Console::detached();
        for name in ["a", "b", "last"] {
            declare(&mut console, name);
        }
        console.enqueue(r#"alias pair "a; b""#, Source::UserInput);
        console.run(&mut NoTarget);

        console.enqueue("pair; last", Source::UserInput);
        assert_eq!(
            recorder(&mut console),
            ["a", "b", "last"],
            "the expansion is inserted at the head, not appended"
        );
    }

    /// An alias that expands to *itself* keeps the queue at length one
    /// forever, so the buffer's cap never sees it. This is the guard that does.
    #[test]
    fn an_alias_that_expands_to_itself_stops_the_round() {
        let mut console = Console::detached();
        console.enqueue("alias loop loop", Source::UserInput);
        console.run(&mut NoTarget);

        console.enqueue("loop", Source::UserInput);
        console.run(&mut NoTarget);

        let text: Vec<String> = console.log().lines().map(|l| l.text.clone()).collect();
        assert!(
            text.iter().any(|l| l.contains("runaway command text")),
            "expected a loud failure, got {text:?}"
        );
        assert!(
            console.buffer().is_empty(),
            "the loop must not resume on the next frame"
        );
    }

    /// The other half: an alias that expands to *many* commands is caught by
    /// the queue cap rather than the round budget.
    #[test]
    fn an_alias_that_expands_without_end_overflows_the_queue() {
        let mut console = Console::detached();
        // The body must be quoted: `add_text` splits on `;` *before* the alias
        // command sees it, so an unquoted body is one command and the rest are
        // run immediately. That is Valve's behaviour and the reason every
        // multi-command alias in a shipped `.cfg` is quoted.
        console.enqueue(r#"alias fan "echo a; fan; fan""#, Source::UserInput);
        console.run(&mut NoTarget);

        console.enqueue("fan", Source::UserInput);
        console.run(&mut NoTarget);

        let text: Vec<String> = console.log().lines().map(|l| l.text.clone()).collect();
        assert!(
            text.iter()
                .any(|l| l.contains("overflow") || l.contains("runaway command text")),
            "expected a loud failure, got {text:?}"
        );
    }

    // ---- exec -------------------------------------------------------------

    #[test]
    fn exec_runs_each_line_and_appends_the_default_extension() {
        let mut console = console_with(
            FakeConfigs::new(&[("cfg/test.cfg", "one\ntwo\nthree")]),
            &[],
        );
        for name in ["one", "two", "three"] {
            declare(&mut console, name);
        }
        console.enqueue("exec test", Source::Code);
        assert_eq!(recorder(&mut console), ["one", "two", "three"]);
    }

    #[test]
    fn an_existing_extension_is_left_alone_so_valve_rc_works() {
        let mut console = console_with(
            FakeConfigs::new(&[("cfg/valve.rc", "stuffcmds")]),
            &["game", "+map", "sp_a1_intro1"],
        );
        declare(&mut console, "map");
        console.enqueue("exec valve.rc", Source::Code);
        assert_eq!(recorder(&mut console), ["map sp_a1_intro1"]);
    }

    /// The behaviour `valve.rc` depends on: a nested `exec` finishes entirely
    /// before the outer file's next line runs.
    #[test]
    fn a_nested_exec_completes_before_the_outer_file_continues() {
        let mut console = console_with(
            FakeConfigs::new(&[
                ("cfg/outer.cfg", "before\nexec inner\nafter"),
                ("cfg/inner.cfg", "inner_one\ninner_two"),
            ]),
            &[],
        );
        for name in ["before", "after", "inner_one", "inner_two"] {
            declare(&mut console, name);
        }
        console.enqueue("exec outer", Source::Code);
        assert_eq!(
            recorder(&mut console),
            ["before", "inner_one", "inner_two", "after"]
        );
    }

    #[test]
    fn a_bad_line_does_not_stop_the_lines_after_it() {
        let mut console = console_with(
            FakeConfigs::new(&[("cfg/test.cfg", "good\nnonsense_command\ngood")]),
            &[],
        );
        declare(&mut console, "good");
        console.enqueue("exec test", Source::Code);
        assert_eq!(recorder(&mut console), ["good", "good"]);
    }

    /// `engine/cmd.cpp:572`. Portal 2 ships neither `autoexec.cfg` nor
    /// `joystick.cfg`, and `valve.rc` execs both.
    #[test]
    fn the_three_optional_configs_fail_silently_and_others_do_not() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.enqueue(
            "exec autoexec.cfg; exec joystick.cfg; exec game.cfg",
            Source::Code,
        );
        console.run(&mut NoTarget);
        assert!(console.log().is_empty(), "a launch must not print these");

        console.enqueue("exec missing.cfg", Source::Code);
        console.run(&mut NoTarget);
        let text: Vec<String> = console.log().lines().map(|l| l.text.clone()).collect();
        assert!(text.iter().any(|l| l.contains("couldn't exec")), "{text:?}");
    }

    #[test]
    fn dangerous_extensions_are_refused() {
        assert!(is_valid_config_extension("valve.rc"));
        assert!(is_valid_config_extension("config.cfg"));
        assert!(!is_valid_config_extension("payload.exe"));
        assert!(
            !is_valid_config_extension("PAYLOAD.EXE"),
            "case-insensitive"
        );
        assert!(!is_valid_config_extension("x.dll"));
    }

    #[test]
    fn exec_refuses_to_recurse_without_end() {
        let mut console = console_with(FakeConfigs::new(&[("cfg/loop.cfg", "exec loop")]), &[]);
        console.enqueue("exec loop", Source::Code);
        console.run(&mut NoTarget);

        let text: Vec<String> = console.log().lines().map(|l| l.text.clone()).collect();
        assert!(text.iter().any(|l| l.contains("refusing")), "{text:?}");
    }

    #[test]
    fn exec_refuses_a_file_over_a_megabyte() {
        let big = "echo x\n".repeat(200_000);
        let mut console = console_with(FakeConfigs::new(&[("cfg/big.cfg", big.as_str())]), &[]);
        console.enqueue("exec big", Source::Code);
        console.run(&mut NoTarget);
        let text: Vec<String> = console.log().lines().map(|l| l.text.clone()).collect();
        assert!(
            text.iter().any(|l| l.contains("larger than 1 MB")),
            "{text:?}"
        );
    }

    // ---- the command line -------------------------------------------------

    #[test]
    fn stuffcmds_turns_plus_arguments_into_commands() {
        let mut console = console_with(
            FakeConfigs::new(&[]),
            &["game", "-window", "+map", "sp_a1_intro1", "+fps_max", "60"],
        );
        declare(&mut console, "map");
        let fps_max = console.cvar("fps_max", "300", CvarFlags::NONE, "");

        console.enqueue("stuffcmds", Source::Code);
        assert_eq!(recorder(&mut console), ["map sp_a1_intro1"]);
        assert_eq!(fps_max.float(), 60.0);
    }

    #[test]
    fn a_valueless_option_does_not_eat_the_next_command() {
        // `CCommandLine::ParmValue` (`tier0/commandline.cpp:646`) refuses a
        // value that starts with `-` or `+`. Without that, `-window` would
        // swallow `+map` and the map would never load.
        let mut console = console_with(
            FakeConfigs::new(&[]),
            &["game", "-window", "+map", "sp_a1_intro1"],
        );
        declare(&mut console, "map");
        console.enqueue("stuffcmds", Source::Code);
        assert_eq!(recorder(&mut console), ["map sp_a1_intro1"]);
    }

    #[test]
    fn a_plus_argument_with_no_value_is_a_bare_command() {
        let mut console = console_with(FakeConfigs::new(&[]), &["game", "+quit"]);
        declare(&mut console, "quit");
        console.enqueue("stuffcmds", Source::Code);
        assert_eq!(recorder(&mut console), ["quit"]);
    }

    #[test]
    fn map_takes_a_second_argument_from_the_command_line() {
        let mut console = console_with(
            FakeConfigs::new(&[]),
            &["game", "+map", "sp_a1_intro1", "reslist"],
        );
        declare(&mut console, "map");
        console.enqueue("stuffcmds", Source::Code);
        assert_eq!(recorder(&mut console), ["map sp_a1_intro1 reslist"]);
    }

    /// `CCvar::GetCommandLineValue` — a *different* path from `stuffcmds`, and
    /// the reason `+fps_max 60` is in effect before `valve.rc` runs.
    #[test]
    fn a_plus_argument_seeds_a_cvar_at_registration() {
        let mut console = console_with(FakeConfigs::new(&[]), &["game", "+fps_max", "60"]);
        let fps_max = console.cvar("fps_max", "300", CvarFlags::NONE, "");
        assert_eq!(
            fps_max.float(),
            60.0,
            "seeded at registration, before anything ran"
        );
        assert!(console.buffer().is_empty(), "and without queuing a command");
    }

    // ---- the whole boot path ----------------------------------------------

    /// Stage 1's deliverable, end to end: the map loads *through* the config
    /// files rather than from a hard-coded launcher branch.
    #[test]
    fn valve_rc_boots_a_map_through_stuffcmds() {
        let mut console = console_with(
            FakeConfigs::new(&[(
                "cfg/valve.rc",
                "exec joystick.cfg\nexec autoexec.cfg\nstuffcmds\nstartupmenu\n",
            )]),
            &["game", "-window", "+map", "sp_a1_intro1"],
        );
        declare(&mut console, "map");

        console.enqueue("exec valve.rc", Source::Code);
        assert_eq!(recorder(&mut console), ["map sp_a1_intro1"]);
        assert_eq!(
            console.take_unknown_count(),
            1,
            "`startupmenu` is GameUI's and is not ported"
        );
    }

    // ---- config persistence (stage 3) --------------------------------------

    #[test]
    fn only_archived_cvars_are_written_and_they_are_sorted() {
        let mut console = Console::detached();
        console.cvar("zzz_archived", "1", CvarFlags::ARCHIVE, "");
        console.cvar("aaa_archived", "2", CvarFlags::ARCHIVE, "");
        console.cvar("Mid_Archived", "3", CvarFlags::ARCHIVE, "");
        console.cvar("not_archived", "4", CvarFlags::NONE, "");

        let mut out = String::new();
        write_archived_cvars(console.cvars(), &mut out);

        assert_eq!(
            out, "aaa_archived \"2\"\nMid_Archived \"3\"\nzzz_archived \"1\"\n",
            "sorted case-insensitively, and FCVAR_ARCHIVE only"
        );
    }

    /// The format is fixed (§7): we write it *and* read it, but a user's
    /// existing `config.cfg` was written by the shipped engine.
    #[test]
    fn a_written_cvar_reads_back_through_the_set_path() {
        let mut console = Console::detached();
        let hostname = console.cvar("hostname", "", CvarFlags::ARCHIVE, "");
        hostname.set_string("  a b  ");

        let mut out = String::new();
        write_archived_cvars(console.cvars(), &mut out);
        assert_eq!(out, "hostname \"  a b  \"\n");

        // What `exec` does with that line: tokenize, then strip the quotes the
        // writer added -- interior spaces intact.
        let cmd = Command::parse(out.trim_end(), Source::Code);
        assert_eq!(strip_set_value(cmd.tail()), "  a b  ");
    }

    #[test]
    fn execifexists_is_silent_about_a_missing_file() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.enqueue("execifexists nothing_here.cfg", Source::Code);
        console.run(&mut NoTarget);
        assert!(console.log().is_empty());

        // Plain `exec` still complains, which is the difference between them.
        console.enqueue("exec nothing_here.cfg", Source::Code);
        console.run(&mut NoTarget);
        assert_eq!(console.log().len(), 1);
    }

    #[test]
    fn the_config_read_flag_starts_false() {
        let mut console = Console::detached();
        assert!(!console.config_was_read());
        console.set_config_was_read(true);
        assert!(console.config_was_read());
    }

    // ---- completion (stage 4) ---------------------------------------------

    fn completing_console() -> Console<'static> {
        let mut console = Console::new(
            FakeConfigs::new(&[
                ("cfg/valve.rc", ""),
                ("cfg/config_default.cfg", ""),
                ("cfg/chapter1.cfg", ""),
                ("cfg/notes.txt", ""),
            ]),
            Vec::new(),
        );
        console.log_mut().set_echo_to_stderr(false);
        for spec in [
            CommandSpec::new("mat_wireframe_toggle", "Toggle wireframe."),
            CommandSpec::new("map", "Load a map.").with_completion(Completion::Files {
                dir: "maps",
                ext: "bsp",
            }),
            CommandSpec::new("secret", "").with_flags(CommandFlags::HIDDEN),
            CommandSpec::new("internal", "").with_flags(CommandFlags::DEVELOPMENTONLY),
        ] {
            console.register_command(spec).expect("unique");
        }
        console.cvar("mat_wireframe", "0", CvarFlags::NONE, "Draw wireframe.");
        console.cvar("mat_luxels", "0", CvarFlags::HIDDEN, "");
        console
    }

    fn texts(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn completion_prefix_matches_commands_and_cvars_and_sorts_them() {
        let console = completing_console();
        let found = console.complete("mat_");
        assert_eq!(
            texts(&found),
            ["mat_wireframe", "mat_wireframe_toggle"],
            "sorted by name; `mat_luxels` is HIDDEN"
        );
        assert_eq!(
            found[0].value.as_deref(),
            Some("0"),
            "a cvar carries its current value; a command does not"
        );
        assert_eq!(found[1].value, None);
    }

    #[test]
    fn completion_hides_developmentonly_and_hidden() {
        let console = completing_console();
        assert!(console.complete("secret").is_empty());
        assert!(console.complete("internal").is_empty());
        assert!(console.complete("mat_lux").is_empty());
    }

    #[test]
    fn an_empty_line_completes_to_nothing_because_history_is_the_uis() {
        assert!(completing_console().complete("").is_empty());
    }

    /// `CommandMatchesText`'s substring mode: once there is a space and no
    /// command claimed the line, every space-separated piece has to appear
    /// somewhere in the name.
    #[test]
    fn a_space_with_no_claiming_command_matches_substrings() {
        let console = completing_console();
        assert_eq!(
            texts(&console.complete("wire frame")),
            ["mat_wireframe", "mat_wireframe_toggle"]
        );
        assert!(
            console.complete("wire nope").is_empty(),
            "every piece has to match, not any"
        );
    }

    /// `exec ` completes from `cfg/*.cfg`, extension stripped, whole-line
    /// replacement — `CBaseAutoCompleteFileList::AutoCompletionFunc`.
    #[test]
    fn a_command_that_claims_its_arguments_completes_files() {
        let console = completing_console();
        assert_eq!(
            texts(&console.complete("exec c")),
            ["exec chapter1", "exec config_default"],
            "sorted, `.cfg` stripped, `notes.txt` not a candidate"
        );
        assert_eq!(
            texts(&console.complete("exec ")),
            ["exec chapter1", "exec config_default"],
            "an empty argument offers everything the extension allows -- and \
             `valve.rc` is not a `.cfg`, which is Valve's list too"
        );
    }

    /// A `Completion::Files` command whose directory has nothing in it falls
    /// back to no suggestions rather than to the name search, which would
    /// otherwise offer `map` for `map de_`.
    #[test]
    fn a_claiming_command_with_no_files_offers_nothing() {
        let console = completing_console();
        assert!(console.complete("map de_").is_empty());
    }

    #[test]
    fn the_completion_list_is_capped() {
        let mut console = Console::detached();
        for index in 0..MAX_COMPLETION_ITEMS + 20 {
            console.cvar(
                Box::leak(format!("test_cvar_{index:03}").into_boxed_str()),
                "0",
                CvarFlags::NONE,
                "",
            );
        }
        assert_eq!(console.complete("test_cvar_").len(), MAX_COMPLETION_ITEMS);
    }

    #[test]
    fn never_as_string_cvars_show_a_number_rather_than_their_string() {
        let mut console = Console::detached();
        console.cvar("test_bits", "3.5", CvarFlags::NEVER_AS_STRING, "");
        console.cvar("test_whole", "2", CvarFlags::NEVER_AS_STRING, "");
        let found = console.complete("test_");
        assert_eq!(found[0].value.as_deref(), Some("3.5"));
        assert_eq!(
            found[1].value.as_deref(),
            Some("2"),
            "an integral value is not printed as 2.000000"
        );
    }
    // ---- the list commands ------------------------------------------------

    /// Everything the console has printed, oldest first.
    fn printed(console: &Console) -> Vec<String> {
        console
            .log()
            .lines()
            .map(|line| line.text.clone())
            .collect()
    }

    /// A [`ConfigFiles`] that remembers what was written through it, so that
    /// `cvarlist log` can be tested without touching a disk.
    #[derive(Default)]
    struct RecordingConfigs {
        written: std::sync::Mutex<Map<String, String>>,
        refuse: bool,
    }

    impl ConfigFiles for std::sync::Arc<RecordingConfigs> {
        fn read_config(&self, _path: &str, _path_id: Option<&str>) -> Option<Vec<u8>> {
            None
        }

        fn write_config(&self, path: &str, contents: &str) -> Result<(), String> {
            if self.refuse {
                return Err("read-only".to_string());
            }
            self.written
                .lock()
                .expect("not poisoned")
                .insert(path.to_string(), contents.to_string());
            Ok(())
        }
    }

    /// A console holding one of each kind of listable thing.
    fn listing_console() -> Console<'static> {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.cvar("test_speed", "5", CvarFlags::ARCHIVE, "How fast it goes.");
        console.cvar("test_secret", "1", CvarFlags::HIDDEN, "Not for listings.");
        console.cvar("test_early", "1", CvarFlags::DEVELOPMENTONLY, "Nor this.");
        console.cvar("test_noclip", "0", CvarFlags::CHEAT, "Walk through walls.");
        declare(&mut console, "test_dance");
        console
    }

    /// The banner, the rows and the footer, and that `DEVELOPMENTONLY` and
    /// `HIDDEN` never reach any of them.
    #[test]
    fn cvarlist_prints_a_banner_rows_and_a_count() {
        let mut console = listing_console();
        console.enqueue("cvarlist test_", Source::UserInput);
        console.run(&mut NoTarget);

        let text = printed(&console);
        assert_eq!(text[0], "cvar list");
        assert_eq!(text[1], "--------------");

        let rows: Vec<&String> = text[2..text.len() - 2].iter().collect();
        let names: Vec<&str> = rows
            .iter()
            .map(|row| row.split(' ').next().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            ["test_dance", "test_noclip", "test_speed"],
            "sorted, and neither the hidden nor the developmentonly cvar: {text:?}"
        );

        assert!(rows[2].contains(", a"), "the archive flag: {:?}", rows[2]);
        assert!(rows[2].contains("How fast it goes."), "{:?}", rows[2]);
        assert!(
            rows[0].contains(" cmd "),
            "a command's value column: {:?}",
            rows[0]
        );

        assert_eq!(text[text.len() - 2], "--------------");
        assert_eq!(text[text.len() - 1], "  3 convars/concommands for [test_]");
    }

    /// With no argument the prefix is empty, which matches everything — and
    /// the footer says "total" rather than naming a filter.
    #[test]
    fn cvarlist_with_no_argument_lists_everything() {
        let mut console = listing_console();
        console.enqueue("cvarlist", Source::UserInput);
        console.run(&mut NoTarget);

        let text = printed(&console);
        let footer = text.last().expect("a footer");
        assert!(footer.ends_with("total convars/concommands"), "{footer}");
        assert!(
            text.iter().any(|line| line.starts_with("exec ")),
            "the built-ins are listed too: {text:?}"
        );
        assert!(
            !text.iter().any(|line| line.starts_with("test_secret")),
            "{text:?}"
        );
    }

    /// `ConCommandBaseLessFunc` drops a leading `+`/`-`, so the two halves of
    /// a button command sort next to each other rather than under punctuation.
    #[test]
    fn cvarlist_sorts_the_two_halves_of_a_button_together() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.cvar("test_fps", "0", CvarFlags::NONE, "");
        declare(&mut console, "+test_forward");
        declare(&mut console, "-test_forward");
        // Not `cvarlist test`: the prefix filter compares from the first
        // character, `+` included, so it would exclude both halves — which is
        // what the shipped engine does too.
        console.enqueue("cvarlist", Source::UserInput);
        console.run(&mut NoTarget);

        let names: Vec<String> = printed(&console)
            .iter()
            .map(|row| row.split(' ').next().unwrap_or_default().to_string())
            .filter(|name| name.contains("test_"))
            .collect();
        assert_eq!(
            names,
            ["+test_forward", "-test_forward", "test_fps"],
            "`forward` sorts before `fps`, and the punctuation is not compared"
        );
    }

    #[test]
    fn cvarlist_help_is_a_usage_line() {
        let mut console = listing_console();
        console.enqueue("cvarlist ?", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(printed(&console), ["cvarlist:  [log logfile] [ partial ]"]);
    }

    #[test]
    fn cvarlist_log_writes_a_csv_beside_the_listing() {
        let files = std::sync::Arc::new(RecordingConfigs::default());
        let mut console = console_with(Box::new(std::sync::Arc::clone(&files)), &[]);
        console.cvar(
            "test_speed",
            "5",
            CvarFlags::ARCHIVE,
            "How \"fast\" it goes.",
        );
        console.enqueue("cvarlist log cvars.csv test_", Source::UserInput);
        console.run(&mut NoTarget);

        let written = files.written.lock().expect("not poisoned");
        let csv = written.get("cvars.csv").expect("the file was written");
        let rows: Vec<&str> = csv.lines().collect();
        assert!(rows[0].starts_with("\"Name\",\"Value\","), "{}", rows[0]);
        assert!(rows[0].ends_with(",\"Help Text\""), "{}", rows[0]);
        assert_eq!(rows.len(), 2, "a header and one cvar: {rows:?}");
        assert!(rows[1].starts_with("\"test_speed\",\"5\","), "{}", rows[1]);
        assert!(
            rows[1].contains("\"archive\""),
            "the flag column is filled in: {}",
            rows[1]
        );
        assert!(
            rows[1].ends_with("\"How 'fast' it goes.\""),
            "a quote inside a field would end it: {}",
            rows[1]
        );

        assert!(
            printed(&console).iter().any(|line| line == "cvar list"),
            "logging does not replace the console output"
        );
    }

    /// A file that will not open stops the command, rather than logging half
    /// of it — which is why `CvarList` opens its handle before it prints.
    #[test]
    fn cvarlist_log_that_cannot_be_written_prints_nothing_else() {
        let files = std::sync::Arc::new(RecordingConfigs {
            refuse: true,
            ..RecordingConfigs::default()
        });
        let mut console = console_with(Box::new(std::sync::Arc::clone(&files)), &[]);
        console.enqueue("cvarlist log /nowhere.csv", Source::UserInput);
        console.run(&mut NoTarget);

        let text = printed(&console);
        assert_eq!(text.len(), 1, "{text:?}");
        assert!(text[0].starts_with("Couldn't open"), "{text:?}");
    }

    /// The log path reaches the same place `exec`'s does, so it gets the same
    /// blocklist — which Valve does not apply here.
    #[test]
    fn cvarlist_log_refuses_a_dangerous_extension() {
        let files = std::sync::Arc::new(RecordingConfigs::default());
        let mut console = console_with(Box::new(std::sync::Arc::clone(&files)), &[]);
        console.enqueue("cvarlist log bin/evil.DLL", Source::Code);
        console.run(&mut NoTarget);

        assert!(files.written.lock().expect("not poisoned").is_empty());
        let text = printed(&console);
        assert_eq!(text.len(), 1, "{text:?}");
        assert!(text[0].contains("invalid file type"), "{text:?}");
    }

    #[test]
    fn help_describes_a_cvar_a_command_and_neither() {
        let mut console = listing_console();
        console.enqueue(
            "help test_speed; help test_dance; help nothing",
            Source::UserInput,
        );
        console.run(&mut NoTarget);

        let text = printed(&console);
        assert!(
            text[0].starts_with("\"test_speed\" = \"5\""),
            "the value, then the flags, then the help: {:?}",
            text[0]
        );
        assert!(text[0].contains(" archive "), "{:?}", text[0]);
        assert!(text[0].ends_with(" - How fast it goes."), "{:?}", text[0]);
        assert_eq!(text[1], "\"test_dance\" ", "a command has no value to show");
        assert_eq!(text[2], "help:  no cvar or command named nothing");
    }

    /// `HIDDEN` means "not discoverable", not "not usable" — so the listings
    /// skip it and `help` does not.
    #[test]
    fn help_finds_a_hidden_cvar_that_the_listings_hide() {
        let mut console = listing_console();
        console.enqueue("help test_secret", Source::UserInput);
        console.run(&mut NoTarget);
        let text = printed(&console);
        assert!(text[0].starts_with("\"test_secret\""), "{text:?}");
        assert!(text[0].contains(" hidden"), "{text:?}");
    }

    /// A cvar's `( def. )` clause appears only once the value has moved, and
    /// the bounds print as `%f`, both of which are what makes two `help` lines
    /// comparable with the shipped engine's.
    #[test]
    fn help_shows_the_default_and_the_bounds_once_the_value_moves() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.cvar_bounded("test_gamma", "2", CvarFlags::NONE, "", Some(1.0), Some(3.0));
        console.enqueue(
            "help test_gamma; test_gamma 3; help test_gamma",
            Source::UserInput,
        );
        console.run(&mut NoTarget);

        let text = printed(&console);
        assert_eq!(
            text[0],
            "\"test_gamma\" = \"2\" min. 1.000000 max. 3.000000"
        );
        assert_eq!(
            text.last().expect("a second description"),
            "\"test_gamma\" = \"3\" ( def. \"2\" ) min. 1.000000 max. 3.000000"
        );
    }

    /// Each search term may match the name *or* the help text, and **every**
    /// term has to match something.
    #[test]
    fn find_requires_every_term_and_looks_in_the_help_text() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.cvar("test_wireframe", "0", CvarFlags::NONE, "Draw the edges.");
        console.cvar("test_vsync", "1", CvarFlags::NONE, "Wait for the display.");
        console.cvar("test_hidden", "1", CvarFlags::HIDDEN, "Draw the edges.");

        console.enqueue("find edges", Source::UserInput);
        console.run(&mut NoTarget);
        let text = printed(&console);
        assert_eq!(
            text.len(),
            1,
            "help text is searched, and HIDDEN is not: {text:?}"
        );
        assert!(text[0].starts_with("\"test_wireframe\""), "{text:?}");

        console.log_mut().clear();
        console.enqueue("find test draw", Source::UserInput);
        console.run(&mut NoTarget);
        let text = printed(&console);
        assert_eq!(
            text.len(),
            1,
            "`test` from the name, `draw` from the help: {text:?}"
        );

        console.log_mut().clear();
        console.enqueue("find test nothing", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(printed(&console).is_empty(), "one term missing is no match");
    }

    #[test]
    fn find_searches_commands_as_well_as_cvars() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.enqueue("find stuffcmds", Source::UserInput);
        console.run(&mut NoTarget);
        let text = printed(&console);
        assert!(text[0].starts_with("\"stuffcmds\" "), "{text:?}");
    }

    #[test]
    fn differences_lists_only_what_has_moved() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.cvar("test_moved", "0", CvarFlags::NONE, "");
        console.cvar("test_still", "0", CvarFlags::NONE, "");
        console.enqueue("test_moved 1; differences", Source::UserInput);
        console.run(&mut NoTarget);

        let text = printed(&console);
        assert_eq!(text.len(), 1, "{text:?}");
        assert!(text[0].starts_with("\"test_moved\" = \"1\""), "{text:?}");
    }

    /// The reason `describe::value` exists rather than [`Cvar::string`]: an
    /// `FCVAR_NEVER_AS_STRING` cvar never updates its string, so comparing
    /// that would report every one of them as unchanged for ever.
    #[test]
    fn differences_sees_a_never_as_string_cvar_move() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        console.cvar("test_bits", "0", CvarFlags::NEVER_AS_STRING, "");
        console.enqueue("differences", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(printed(&console).is_empty(), "still at its default");

        console.enqueue("test_bits 2; differences", Source::UserInput);
        console.run(&mut NoTarget);
        let text = printed(&console);
        assert_eq!(text.len(), 1, "{text:?}");
        assert!(text[0].starts_with("\"test_bits\" = \"2\""), "{text:?}");
    }

    #[test]
    fn toggle_with_no_values_flips_between_zero_and_one() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        let flag = console.cvar("test_flag", "0", CvarFlags::NONE, "");

        console.enqueue("toggle test_flag", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(flag.int(), 1);

        console.enqueue("toggle test_flag", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(flag.int(), 0);
        assert!(printed(&console)
            .last()
            .expect("the description is printed each time")
            .starts_with("\"test_flag\" = \"0\""),);
    }

    #[test]
    fn toggle_cycles_a_value_list_and_wraps() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        let mode = console.cvar("test_mode", "low", CvarFlags::NONE, "");

        for expected in ["medium", "high", "low"] {
            console.enqueue("toggle test_mode low medium high", Source::UserInput);
            console.run(&mut NoTarget);
            assert_eq!(&*mode.string(), expected);
        }

        // A value that is not in the list starts the cycle at its beginning.
        mode.set_string("other");
        console.enqueue("toggle test_mode low medium high", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(&*mode.string(), "low");
    }

    #[test]
    fn toggle_is_refused_for_a_cheat_cvar_and_for_an_unknown_name() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        let noclip = console.cvar("test_noclip", "0", CvarFlags::CHEAT, "");

        console.enqueue("toggle test_noclip; toggle test_nothing", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(noclip.int(), 0, "no cheating without sv_cheats");

        let text = printed(&console);
        assert!(text[0].starts_with("Can't use cheat cvar"), "{text:?}");
        assert_eq!(text[1], "test_nothing is not a valid cvar");

        console.enqueue("sv_cheats 1; toggle test_noclip", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(noclip.int(), 1);
    }

    /// The wrap at both ends, and that the set arrives **through the buffer**
    /// rather than being written here — which is what makes it appear in a
    /// recording as a set rather than as an increment.
    #[test]
    fn incrementvar_wraps_at_both_ends_and_sets_through_the_buffer() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        let volume = console.cvar("test_volume", "0.9", CvarFlags::NONE, "");

        console.enqueue("incrementvar test_volume 0 1 0.1", Source::UserInput);
        console.run(&mut NoTarget);
        assert!((volume.float() - 1.0).abs() < 0.001, "{}", volume.float());

        console.enqueue("incrementvar test_volume 0 1 0.1", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(volume.float(), 0.0, "past the top wraps to the minimum");

        console.enqueue("incrementvar test_volume 0 1 -0.1", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(volume.float(), 1.0, "below the bottom wraps to the maximum");

        console.enqueue("incrementvar test_volume 0 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert!(printed(&console)
            .last()
            .expect("a usage line")
            .starts_with("Usage: incrementvar"),);
    }

    /// `incrementvar` reaches the cvar by re-entering dispatch, so everything
    /// dispatch does still applies — here, the cheat gate.
    #[test]
    fn incrementvar_goes_through_dispatch_and_so_through_the_cheat_gate() {
        let mut console = console_with(FakeConfigs::new(&[]), &[]);
        let noclip = console.cvar("test_noclip", "0", CvarFlags::CHEAT, "");
        console.enqueue("incrementvar test_noclip 0 1 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(noclip.int(), 0);
        assert!(
            printed(&console)
                .iter()
                .any(|line| line.starts_with("Can't use cheat cvar")),
            "{:?}",
            printed(&console)
        );
    }
}
