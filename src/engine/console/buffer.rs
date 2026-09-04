//! The command buffer: text in, argv out, at a defined point in the frame.
//!
//! `tier1/commandbuffer.cpp`, ported behaviour-for-behaviour because every part
//! of it is load-bearing — `ENGINE_CONSOLE.md` §4.2. The fixed buffers go
//! (`ARGS_BUFFER_LENGTH` 8,192, `COMMAND_MAX_LENGTH` 512, `COMMAND_MAX_ARGC`
//! 64) along with the `Compact()` that serviced them; the scheduling does not.
//!
//! Three behaviours are worth stating before you read the code, because each
//! one looks like an implementation detail and is not:
//!
//! - **Text is stored, tokens are not.** [`CommandBuffer::add_text`] splits on
//!   `;` and newlines and keeps the *text*; tokenizing happens at
//!   [`dequeue`](CommandBuffer::dequeue). That is what makes a delayed command
//!   possible at all.
//! - **`wait` is handled at insert time and the command is dropped**
//!   (`commandbuffer.cpp:236`). It adds its delay to a running tick that the
//!   *remaining commands in the same text* then inherit, which is why `wait` in
//!   a `.cfg` schedules rather than sleeps.
//! - **Insertion during processing goes to the head** (`InsertImmediateCommand`,
//!   `:110`). An alias expanding to three commands runs those three *next*, not
//!   after everything already queued. Get this wrong and aliases execute in a
//!   plausible but wrong order.

use std::collections::VecDeque;

use super::token::{Command, Source};

/// How many commands may be queued before the buffer refuses more.
///
/// Valve's limit was a byte count on a fixed arena. This is a count, and it
/// exists for a different reason: an alias that expands to itself is an
/// infinite loop that inserts at the head, and without a cap it eats memory
/// silently instead of failing. `ENGINE_CONSOLE.md` §4.2 asks for exactly this.
const MAX_QUEUED_COMMANDS: usize = 1024;

#[derive(Debug, Clone)]
struct Queued {
    text: String,
    tick: i32,
    source: Source,
}

/// Queued command text, ordered by the tick it is due on.
///
/// A "tick" here is one [`CommandBuffer::begin_processing`] call, not a server
/// tick: the engine spoofs it by passing 1 every time (`engine/cmd.cpp:288`),
/// which makes `wait 1` mean "next frame". The shipped `.cfg` files assume
/// that, so it is kept.
#[derive(Debug)]
pub struct CommandBuffer {
    /// Sorted by tick. Valve uses an intrusive linked list so that it can link
    /// before an arbitrary node; a `VecDeque` gives the same two operations —
    /// push-front for an immediate insert, ordered insert otherwise — because
    /// dequeuing always takes the head.
    queue: VecDeque<Queued>,
    current_tick: i32,
    last_tick_to_process: i32,
    processing: bool,
    /// Where the next immediate insert goes.
    ///
    /// Valve's `InsertImmediateCommand` links before `m_hNextCommand`, which is
    /// re-pointed at the head only by `BeginProcessingCommands` and
    /// `DequeueNextCommand` — **not by the insert itself**. So several
    /// commands inserted between two dequeues all land before the *same*
    /// anchor node and therefore keep their order. Pushing each one to the
    /// front instead would reverse them, which is the plausible-but-wrong
    /// alias ordering §4.2 warns about.
    immediate_cursor: usize,
    wait_delay_ticks: i32,
    wait_enabled: bool,
    /// Set when a push was refused, so the console can say so once rather than
    /// once per dropped command.
    overflowed: bool,
}

impl Default for CommandBuffer {
    fn default() -> CommandBuffer {
        CommandBuffer::new()
    }
}

impl CommandBuffer {
    pub fn new() -> CommandBuffer {
        CommandBuffer {
            queue: VecDeque::new(),
            current_tick: 0,
            // `m_nLastTickToProcess = -1`.
            last_tick_to_process: -1,
            processing: false,
            immediate_cursor: 0,
            // `m_nWaitDelayTicks = 1` (`commandbuffer.cpp:34`).
            wait_delay_ticks: 1,
            wait_enabled: true,
            overflowed: false,
        }
    }

    /// `CCommandBuffer::SetWaitEnabled`. `wait` is disabled while executing
    /// commands from an untrusted source, since it is the primitive a hostile
    /// `.cfg` would use to stall the engine.
    pub fn set_wait_enabled(&mut self, enabled: bool) {
        self.wait_enabled = enabled;
    }

    pub fn is_processing(&self) -> bool {
        self.processing
    }

    pub fn current_tick(&self) -> i32 {
        self.current_tick
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Takes the overflow flag, if one was raised since it was last taken.
    pub fn take_overflow(&mut self) -> bool {
        std::mem::take(&mut self.overflowed)
    }

    /// `Cbuf_AddText`. Splits `text` into commands and queues them.
    ///
    /// `tick_delay` schedules for `current_tick + tick_delay`. Zero means "this
    /// round", which — while the buffer is processing — means *next*, at the
    /// head. `Cbuf_InsertText` is this with a delay of zero and is not a
    /// separate path: the head-versus-tail decision is made in
    /// [`insert`](CommandBuffer::insert), from whether processing is under way.
    ///
    /// Returns false if anything was dropped for overflow.
    pub fn add_text(&mut self, text: &str, source: Source, tick_delay: i32) -> bool {
        let mut tick = self.current_tick + tick_delay.max(0);
        let mut ok = true;

        for command in split_commands(text) {
            // Only argv[0] and the raw remainder are needed to recognise
            // `wait`; the full tokenize happens at dequeue.
            let parsed = Command::parse(command, source);
            if parsed.is_empty() {
                continue;
            }

            // `wait` never reaches the dispatcher. It moves the tick that the
            // rest of *this* text lands on and is then dropped.
            if self.wait_enabled && parsed.name().eq_ignore_ascii_case("wait") {
                let delay = match parsed.args().first() {
                    Some(arg) => arg.trim().parse().unwrap_or(0),
                    None => self.wait_delay_ticks,
                };
                tick += delay.max(0);
                continue;
            }

            if !self.insert(command, tick, source) {
                ok = false;
            }
        }

        ok
    }

    /// `CCommandBuffer::InsertCommand` (`commandbuffer.cpp:119`).
    fn insert(&mut self, text: &str, tick: i32, source: Source) -> bool {
        if self.queue.len() >= MAX_QUEUED_COMMANDS {
            self.overflowed = true;
            return false;
        }

        let queued = Queued {
            text: text.to_string(),
            tick,
            source,
        };

        if !self.processing || tick > self.current_tick {
            // Ordered by tick, stable within a tick: the first entry strictly
            // later than this one is where it goes.
            let at = self
                .queue
                .iter()
                .position(|q| q.tick > queued.tick)
                .unwrap_or(self.queue.len());
            self.queue.insert(at, queued);
            // Keep the anchor pointing at the same logical element.
            if at <= self.immediate_cursor {
                self.immediate_cursor += 1;
            }
        } else {
            // `InsertImmediateCommand`. The cursor is what makes a three-command
            // alias body run in the order it was written.
            let at = self.immediate_cursor.min(self.queue.len());
            self.queue.insert(at, queued);
            self.immediate_cursor = at + 1;
        }
        true
    }

    /// `BeginProcessingCommands`. Opens the window this round may execute.
    pub fn begin_processing(&mut self, delta_ticks: i32) {
        if delta_ticks == 0 {
            return;
        }
        debug_assert!(!self.processing, "nested begin_processing");
        self.processing = true;
        self.immediate_cursor = 0;
        self.last_tick_to_process = self.current_tick + delta_ticks - 1;
    }

    /// `DequeueNextCommand`. The next command due within the open window.
    ///
    /// Advances the current tick to the dequeued command's, which is what lets
    /// a `wait`-delayed command insert further text relative to *its* tick
    /// rather than the tick the round opened on.
    pub fn dequeue(&mut self) -> Option<Command> {
        debug_assert!(self.processing, "dequeue outside begin/end_processing");
        let front = self.queue.front()?;
        if front.tick > self.last_tick_to_process {
            return None;
        }
        let queued = self.queue.pop_front().expect("front was just observed");
        // `m_hNextCommand = m_Commands.Head()`: the anchor moves to whatever is
        // now in front, so the next command's insertions go ahead of it.
        self.immediate_cursor = 0;
        self.current_tick = queued.tick;
        Some(Command::parse(&queued.text, queued.source))
    }

    /// `EndProcessingCommands` (`commandbuffer.cpp:365`). Closes the window and
    /// discards anything still queued for a tick that has now passed.
    pub fn end_processing(&mut self) {
        if !self.processing {
            return;
        }
        self.processing = false;
        self.immediate_cursor = 0;
        self.current_tick = self.last_tick_to_process + 1;
        let current = self.current_tick;
        self.queue.retain(|q| q.tick >= current);
    }

    /// `DelayAllQueuedCommands`.
    pub fn delay_all(&mut self, delay: i32) {
        if delay <= 0 {
            return;
        }
        for queued in &mut self.queue {
            queued.tick += delay;
        }
    }

    /// Drops everything queued. `Cbuf_Clear`.
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

/// Splits text into commands on `;` and newlines.
///
/// `CCommandBuffer::GetNextCommandLength` (`commandbuffer.cpp:162`) driven by
/// `AddText`'s loop. This is **not** the argv tokenizer — see
/// [`super::token`] — and the two disagree in ways that matter:
///
/// - A `;` inside a quoted string does not split; a **newline does**, even
///   inside quotes. Valve flags that second one in its own source as legacy
///   ("*FIXME: This is legacy behavior; should we not break if a \n is inside a
///   quoted string?*"). It is kept, because shipped `.cfg` files were written
///   against it.
/// - A `//` comment runs to the end of the line and is trimmed off the command,
///   but only the *tail* of it: the characters before it are still the command.
/// - Quote characters are counted in the command text rather than stripped;
///   stripping is the argv tokenizer's job.
pub fn split_commands(text: &str) -> Vec<&str> {
    let mut commands = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;

    while start < bytes.len() {
        let (length, next) = next_command_length(&bytes[start..]);
        if length > 0 {
            commands.push(&text[start..start + length]);
        }
        // Valve advances past the separator unconditionally; when none was
        // found, `next` is the remaining length and this ends the loop.
        start += next + 1;
    }

    commands
}

/// `(command length, offset of the separator)` for the command at the front of
/// `bytes`.
fn next_command_length(bytes: &[u8]) -> (usize, usize) {
    let mut length = 0;
    let mut quoted = false;
    let mut commented = false;
    let mut offset = 0;

    while offset < bytes.len() {
        let c = bytes[offset];

        if !commented {
            if c == b'"' {
                quoted = !quoted;
                // Valve `continue`s here, but the loop's increment still runs,
                // so the quote *is* counted in the command text.
                offset += 1;
                length += 1;
                continue;
            }

            if !quoted && c == b'/' && offset + 1 < bytes.len() && bytes[offset + 1] == b'/' {
                // Everything from here to the newline is comment: it advances
                // the offset but stops adding to the command's length.
                commented = true;
                offset += 2;
                continue;
            }

            if !quoted && c == b';' {
                break;
            }
        }

        if c == b'\n' {
            break;
        }

        offset += 1;
        if !commented {
            length += 1;
        }
    }

    (length, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(buffer: &mut CommandBuffer) -> Vec<String> {
        let mut out = Vec::new();
        buffer.begin_processing(1);
        while let Some(cmd) = buffer.dequeue() {
            out.push(cmd.name().to_string());
        }
        buffer.end_processing();
        out
    }

    #[test]
    fn splits_on_semicolons_and_newlines() {
        assert_eq!(split_commands("echo a; echo b"), ["echo a", " echo b"]);
        assert_eq!(split_commands("echo a\necho b"), ["echo a", "echo b"]);
        assert_eq!(split_commands("echo a;;echo b"), ["echo a", "echo b"]);
        assert!(split_commands("").is_empty());
        assert!(split_commands(";\n;").is_empty());
    }

    #[test]
    fn a_semicolon_inside_quotes_does_not_split() {
        assert_eq!(
            split_commands(r#"say "a;b"; echo c"#),
            [r#"say "a;b""#, " echo c"]
        );
    }

    /// Valve's own FIXME at `commandbuffer.cpp:194`. Reproduced deliberately:
    /// the shipped `.cfg` files were written against this behaviour.
    #[test]
    fn a_newline_splits_even_inside_quotes() {
        assert_eq!(split_commands("say \"a\nb\""), ["say \"a", "b\""]);
    }

    #[test]
    fn comments_are_trimmed_off_the_command_not_the_line() {
        assert_eq!(split_commands("echo hi // trailing"), ["echo hi "]);
        assert_eq!(split_commands("// whole line"), Vec::<&str>::new());
        assert_eq!(split_commands("echo a // c\necho b"), ["echo a ", "echo b"]);
    }

    #[test]
    fn a_comment_inside_quotes_is_not_a_comment() {
        assert_eq!(split_commands(r#"say "http://x""#), [r#"say "http://x""#]);
    }

    #[test]
    fn queues_and_dequeues_in_order() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("echo a; echo b; echo c", Source::Code, 0);
        assert_eq!(buffer.len(), 3);
        assert_eq!(drain(&mut buffer), ["echo", "echo", "echo"]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn wait_defers_the_rest_of_the_same_text() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("map a; wait; map b", Source::Code, 0);
        assert_eq!(buffer.len(), 2, "`wait` itself is dropped, not queued");

        // One round executes only what is due now.
        buffer.begin_processing(1);
        let first = buffer.dequeue().expect("the undelayed command");
        assert_eq!(first.arg(1), Some("a"));
        assert!(buffer.dequeue().is_none(), "the second is a tick away");
        buffer.end_processing();

        buffer.begin_processing(1);
        let second = buffer.dequeue().expect("the delayed command");
        assert_eq!(second.arg(1), Some("b"));
        buffer.end_processing();
    }

    #[test]
    fn wait_takes_an_explicit_count() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("echo now; wait 3; echo later", Source::Code, 0);

        assert_eq!(drain(&mut buffer), ["echo"], "tick 0");
        assert_eq!(drain(&mut buffer), Vec::<String>::new(), "tick 1");
        assert_eq!(drain(&mut buffer), Vec::<String>::new(), "tick 2");
        assert_eq!(drain(&mut buffer), ["echo"], "tick 3");
    }

    #[test]
    fn wait_can_be_disabled() {
        let mut buffer = CommandBuffer::new();
        buffer.set_wait_enabled(false);
        buffer.add_text("echo a; wait; echo b", Source::Code, 0);
        // With `wait` disabled it is an ordinary command and reaches dispatch,
        // where it will be reported as unknown.
        assert_eq!(drain(&mut buffer), ["echo", "wait", "echo"]);
    }

    /// The alias-ordering behaviour, in the buffer where it actually lives.
    #[test]
    fn insertion_during_processing_goes_to_the_head() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("first; last", Source::Code, 0);

        buffer.begin_processing(1);
        let first = buffer.dequeue().expect("first");
        assert_eq!(first.name(), "first");

        // What an alias expansion does.
        buffer.add_text("expanded_a; expanded_b", Source::Code, 0);

        let mut order = vec![first.name().to_string()];
        while let Some(cmd) = buffer.dequeue() {
            order.push(cmd.name().to_string());
        }
        buffer.end_processing();

        assert_eq!(
            order,
            ["first", "expanded_a", "expanded_b", "last"],
            "the expansion runs next, not after everything already queued"
        );
    }

    /// Three commands inserted between two dequeues keep their written order.
    #[test]
    fn several_immediate_inserts_keep_their_order() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("head; tail", Source::Code, 0);
        buffer.begin_processing(1);
        buffer.dequeue();
        buffer.add_text("one; two; three", Source::Code, 0);

        let mut order = Vec::new();
        while let Some(cmd) = buffer.dequeue() {
            order.push(cmd.name().to_string());
        }
        buffer.end_processing();
        assert_eq!(order, ["one", "two", "three", "tail"]);
    }

    #[test]
    fn a_delayed_insert_during_processing_still_sorts_by_tick() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("a", Source::Code, 0);
        buffer.begin_processing(1);
        buffer.dequeue();
        // Explicitly later: not immediate, so it must not jump the queue.
        buffer.add_text("later", Source::Code, 5);
        assert!(buffer.dequeue().is_none());
        buffer.end_processing();
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn the_queue_is_capped_so_a_runaway_alias_fails_loudly() {
        let mut buffer = CommandBuffer::new();
        for _ in 0..MAX_QUEUED_COMMANDS + 10 {
            buffer.add_text("echo x", Source::Code, 0);
        }
        assert_eq!(buffer.len(), MAX_QUEUED_COMMANDS);
        assert!(buffer.take_overflow(), "overflow is reported");
        assert!(!buffer.take_overflow(), "and only once");
    }

    #[test]
    fn stale_commands_are_dropped_when_the_round_closes() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("a", Source::Code, 0);
        buffer.begin_processing(1);
        // Never dequeued: the round closes with it still queued for tick 0.
        buffer.end_processing();
        assert!(buffer.is_empty());
    }

    #[test]
    fn source_survives_the_queue() {
        let mut buffer = CommandBuffer::new();
        buffer.add_text("bind w +forward", Source::UserInput, 0);
        buffer.begin_processing(1);
        let cmd = buffer.dequeue().expect("queued");
        assert_eq!(cmd.source(), Source::UserInput);
        buffer.end_processing();
    }
}
