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

// The module is complete against stage 1 of `portdocs/ENGINE_CONSOLE.md` §8,
// and stage 1 is deliberately larger than its callers: `Source`'s remote
// variants are the security model §4.7 says to port *before* `net/` needs it,
// `Completion` is declared with the commands that will be completed in stage 4,
// and `Cvar`'s typed setters, the log's accessors and `NoTarget`/`NoConfigFiles`
// are the surface the next four stages and the tests call.
//
// **Remove this once the console UI (stage 4) lands** — at that point the ring,
// the completion data and the input path all have real consumers, and anything
// still unused here is genuinely dead.
#![allow(dead_code)]

pub mod buffer;
pub mod cvar;
pub mod log;
pub mod token;

use std::collections::HashMap;

pub use buffer::CommandBuffer;
pub use cvar::{CommandFlags, Cvar, CvarFlags, CvarRegistry, RegisterError};
// `Color` and `Line` are the scrollback's shape, re-exported for the console
// UI (stage 4) that will render it. Nothing in the crate reads them yet.
#[allow(unused_imports)]
pub use log::{Color, Line, Log};
pub use token::{Command, Source};

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
    /// Commands dispatched in the current [`Console::run`]. See
    /// [`MAX_COMMANDS_PER_ROUND`].
    dispatched: u32,
    budget_exceeded: bool,
    /// `ENGINE_CONSOLE.md` §9 open question 6: shipped `.cfg` files name cvars
    /// from subsystems that do not exist yet, and a wall of errors at every
    /// launch is worse than a count.
    unknown_from_code: u32,
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
            dispatched: 0,
            budget_exceeded: false,
            unknown_from_code: 0,
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

    /// How many `Source::Code` commands went unrecognised since the last call,
    /// resetting the count. See [`Console::unknown_from_code`].
    pub fn take_unknown_count(&mut self) -> u32 {
        std::mem::take(&mut self.unknown_from_code)
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
            let Console { buffer, log, .. } = self;
            let mut cx = ExecContext {
                buffer,
                log,
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

    /// `Unknown command "%s"`, quietly when it came from a config file.
    ///
    /// §9 open question 6: `modsettings.cfg` and `config_default.cfg` are
    /// exec'd unconditionally and name cvars from subsystems that do not exist
    /// yet, so a printed error per line is a wall at every launch. A typed
    /// command is different — silence there just looks broken.
    fn report_unknown(&mut self, cmd: &Command) {
        if cmd.source() == Source::Code {
            self.unknown_from_code += 1;
            self.log
                .developer_print(1, &format!("Unknown command \"{}\"", cmd.name()));
        } else {
            self.log
                .error(&format!("Unknown command \"{}\"", cmd.name()));
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
            let line = describe_cvar(&cvar);
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
            "exec" => self.cmd_exec(cmd, target),
            "alias" => self.cmd_alias(cmd),
            "echo" => {
                let text = cmd.args().join(" ");
                self.log.echo(&text);
            }
            "clear" => self.log.clear(),
            "stuffcmds" => self.cmd_stuffcmds(cmd),
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
    fn cmd_exec(&mut self, cmd: &Command, target: &mut dyn CommandTarget) {
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
            if !fails_silently(&name) {
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

/// `ConVar_PrintDescription`, shortened to the line that matters.
fn describe_cvar(cvar: &Cvar) -> String {
    let value = match cvar.flags().contains(CvarFlags::NEVER_AS_STRING) {
        true => cvar.float().to_string(),
        false => cvar.string().to_string(),
    };
    let mut line = format!("\"{}\" = \"{}\"", cvar.name(), value);
    if cvar.default_value() != value {
        line.push_str(&format!(" ( def. \"{}\" )", cvar.default_value()));
    }
    match cvar.bounds() {
        (Some(min), Some(max)) => line.push_str(&format!(" min. {min} max. {max}")),
        (Some(min), None) => line.push_str(&format!(" min. {min}")),
        (None, Some(max)) => line.push_str(&format!(" max. {max}")),
        (None, None) => {}
    }
    if !cvar.help().is_empty() {
        line.push('\n');
        line.push_str(cvar.help());
    }
    line
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

        console.enqueue("not_a_thing 1", Source::UserInput);
        console.run(&mut NoTarget);
        assert_eq!(console.log().len(), 1, "a typed mistake is always visible");
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
}
