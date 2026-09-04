//! Console output: the funnel every message goes through, and the ring it
//! lands in.
//!
//! `Con_ColorPrint` (`engine/console.cpp:767`) fanned out to six places — the
//! net console, the VGui console, the notify area, the debugger, the
//! `con_logfile` file, and `tier0`'s spew. Three of those are deleted
//! (`ENGINE_CONSOLE.md` §5: the net console is unauthenticated remote
//! execution, the notify area is a HUD element, `tier0`'s logging channels are
//! replaced wholesale), one is `egui`'s later, and what is left is a bounded
//! ring plus the `eprintln!` the port already does.
//!
//! The ring exists now rather than with the UI on purpose. `ENGINE_CONSOLE.md`
//! §0.7: retrofitting scrollback into every call site later is worse than
//! having somewhere to put it from the start.
//!
//! # Not `tracing`, yet
//!
//! §9 open question 3 weighs a `tracing` `Layer` feeding this ring against a
//! `VecDeque<Line>`, and settles on hand-rolled for now: `developer` and
//! `con_filter_*` are recognisably level-and-target filtering, but the port has
//! forty `eprintln!` sites and no other structured-output need. **The trigger
//! for revisiting is a second consumer** — a log file with levels, or
//! per-subsystem filtering that `con_filter_text` cannot express.

use std::collections::VecDeque;

use super::cvar::Cvar;

/// How a line was produced. The UI stage colours by this; `Con_ColorPrint`
/// took an RGBA directly, which is a decision the caller should not be making.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// `Con_Printf`.
    Normal,
    /// `Warning`.
    Warning,
    /// `Error`/`ConMsg` failures.
    Error,
    /// `Con_DPrintf`, gated on `developer`.
    Developer,
    /// The text of a command, echoed as it executes.
    Echo,
}

/// One line of scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub color: Color,
    /// `con_filter_enable 2` dims non-matching lines rather than dropping
    /// them, which is the mode worth having: you keep the context around the
    /// thing you were looking for.
    pub dim: bool,
}

/// The console's scrollback and the filters in front of it.
///
/// Holds its own [`Cvar`] handles rather than being handed values or a
/// `&Console`, which is `ENGINE_CONSOLE.md` §6.1 applied to the console's own
/// internals: a subsystem that reads `developer` keeps the cvar.
#[derive(Debug)]
pub struct Log {
    lines: VecDeque<Line>,
    capacity: usize,
    /// Whether to mirror to stderr. On by default — until the UI stage there is
    /// nowhere else for output to go.
    echo_to_stderr: bool,
    developer: Cvar,
    filter_enable: Cvar,
    filter_text: Cvar,
    filter_text_out: Cvar,
}

/// Lines kept before the oldest is dropped.
const DEFAULT_CAPACITY: usize = 1024;

impl Log {
    /// The four cvars are registered by [`Console::new`](super::Console::new)
    /// and cloned in here.
    pub fn new(
        developer: Cvar,
        filter_enable: Cvar,
        filter_text: Cvar,
        filter_text_out: Cvar,
    ) -> Log {
        Log {
            lines: VecDeque::new(),
            capacity: DEFAULT_CAPACITY,
            echo_to_stderr: true,
            developer,
            filter_enable,
            filter_text,
            filter_text_out,
        }
    }

    /// Silences the stderr mirror. Tests want the ring without the noise.
    pub fn set_echo_to_stderr(&mut self, echo: bool) {
        self.echo_to_stderr = echo;
    }

    /// `Con_Printf`.
    pub fn print(&mut self, text: &str) {
        self.emit(Color::Normal, text);
    }

    /// `Warning`.
    pub fn warn(&mut self, text: &str) {
        self.emit(Color::Warning, text);
    }

    /// `ConMsg` on a failed command — an unknown name, a file that would not
    /// open. Not fatal; the engine has no `Error()` that this reaches.
    pub fn error(&mut self, text: &str) {
        self.emit(Color::Error, text);
    }

    /// The command text itself, echoed as it runs.
    pub fn echo(&mut self, text: &str) {
        self.emit(Color::Echo, text);
    }

    /// `Con_DPrintf`, gated on `developer` being at least `level`.
    ///
    /// `developer` is a **level, not a bool** — `developer 2` is meaningfully
    /// noisier than `developer 1` throughout the original, and collapsing it
    /// would throw that away (§4.9).
    pub fn developer_print(&mut self, level: i32, text: &str) {
        if self.developer.int() >= level {
            self.emit(Color::Developer, text);
        }
    }

    /// The current `developer` level, for callers deciding whether to do work
    /// before formatting a message.
    pub fn developer_level(&self) -> i32 {
        self.developer.int()
    }

    /// The scrollback, oldest first. What the `egui` stage renders.
    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// `clear`.
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    fn emit(&mut self, color: Color, text: &str) {
        let Some(dim) = self.filter(text) else {
            return;
        };

        if self.echo_to_stderr {
            eprintln!("{}", text.trim_end_matches('\n'));
        }

        // A line-based ring wants lines: `Con_Printf` callers routinely pass
        // several at once, and a trailing newline is a terminator rather than
        // an empty line.
        for line in text.trim_end_matches('\n').split('\n') {
            if self.lines.len() == self.capacity {
                self.lines.pop_front();
            }
            self.lines.push_back(Line {
                text: line.to_string(),
                color,
                dim,
            });
        }
    }

    /// `con_filter_enable`: 0 off, 1 drop non-matching, 2 dim non-matching.
    ///
    /// `None` drops the line; `Some(dim)` keeps it.
    fn filter(&self, text: &str) -> Option<bool> {
        let mode = self.filter_enable.int();
        if mode <= 0 {
            return Some(false);
        }

        let lowered = text.to_ascii_lowercase();
        let include = self.filter_text.string();
        let exclude = self.filter_text_out.string();

        let mut matches = true;
        if !include.is_empty() {
            matches &= lowered.contains(&include.to_ascii_lowercase());
        }
        if !exclude.is_empty() {
            matches &= !lowered.contains(&exclude.to_ascii_lowercase());
        }

        match (matches, mode) {
            (true, _) => Some(false),
            // Mode 2 keeps the line and marks it; anything else drops it.
            (false, 2) => Some(true),
            (false, _) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::cvar::{Cvar, CvarFlags};
    use super::*;

    fn log() -> Log {
        let mut log = Log::new(
            Cvar::detached("developer", "0", CvarFlags::NONE, ""),
            Cvar::detached("con_filter_enable", "0", CvarFlags::NONE, ""),
            Cvar::detached("con_filter_text", "", CvarFlags::NONE, ""),
            Cvar::detached("con_filter_text_out", "", CvarFlags::NONE, ""),
        );
        log.set_echo_to_stderr(false);
        log
    }

    fn texts(log: &Log) -> Vec<&str> {
        log.lines().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn keeps_lines_in_order() {
        let mut log = log();
        log.print("one");
        log.warn("two");
        assert_eq!(texts(&log), ["one", "two"]);
        assert_eq!(log.lines().next().expect("first").color, Color::Normal);
    }

    #[test]
    fn a_multi_line_message_becomes_several_lines() {
        let mut log = log();
        log.print("a\nb\n");
        assert_eq!(
            texts(&log),
            ["a", "b"],
            "the trailing newline is a terminator"
        );
    }

    #[test]
    fn developer_is_a_level_not_a_bool() {
        let mut log = log();
        log.developer_print(1, "quiet");
        assert!(log.is_empty());

        log.developer.set_int(1);
        log.developer_print(1, "shown at 1");
        log.developer_print(2, "needs 2");
        assert_eq!(texts(&log), ["shown at 1"]);

        log.developer.set_int(2);
        log.developer_print(2, "shown at 2");
        assert_eq!(texts(&log), ["shown at 1", "shown at 2"]);
    }

    #[test]
    fn filter_mode_one_drops_and_mode_two_dims() {
        let mut log = log();
        log.filter_enable.set_int(1);
        log.filter_text.set_string("keep");
        log.print("please keep me");
        log.print("drop me");
        assert_eq!(texts(&log), ["please keep me"]);

        log.clear();
        log.filter_enable.set_int(2);
        log.print("please keep me");
        log.print("dim me");
        assert_eq!(texts(&log), ["please keep me", "dim me"]);
        let dims: Vec<bool> = log.lines().map(|l| l.dim).collect();
        assert_eq!(dims, [false, true]);
    }

    #[test]
    fn filter_text_out_excludes() {
        let mut log = log();
        log.filter_enable.set_int(1);
        log.filter_text_out.set_string("spam");
        log.print("useful");
        log.print("spam spam spam");
        assert_eq!(texts(&log), ["useful"]);
    }

    #[test]
    fn the_ring_is_bounded() {
        let mut log = log();
        log.capacity = 3;
        for i in 0..5 {
            log.print(&i.to_string());
        }
        assert_eq!(texts(&log), ["2", "3", "4"]);
    }
}
