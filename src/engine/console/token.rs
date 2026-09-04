//! One command's text, split into argv.
//!
//! This is `CCommand::Tokenize` (`tier1/convar.cpp:407`) and the
//! `CUtlBuffer::ParseToken` (`tier1/utlbuffer.cpp:1357`) it delegates to.
//! `characterset_t` and its 256-byte lookup table are deleted with them.
//!
//! **There are two splitters in this module and they are not the same one.**
//! `ENGINE_CONSOLE.md` §4.3 calls this the easiest thing here to get subtly
//! wrong. The other one — text into *commands*, on `;` and newlines — is
//! [`super::buffer::split_commands`]. This one turns *one* command into argv,
//! and it treats `;` as an ordinary word character, because by the time it runs
//! the separator is already gone.

/// Where a command came from.
///
/// `cmd_source_t` (`public/tier1/convar.h:88`), and `ENGINE_CONSOLE.md` §4.7
/// explains why all six variants are here when only two can occur: this is a
/// **security model**, not bookkeeping. The flags in
/// [`CvarFlags`](super::CvarFlags) are read *against* the source — a
/// `clc_stringcmd` from a connected client may only run commands marked for
/// the game DLL, and Valve's own comment on the demo variant reads "*Should be
/// heavily restricted as demo commands can come from untrusted sources*".
///
/// Porting the enum now costs nothing. The alternative is retrofitting a
/// provenance field through a dispatcher that has spent a year assuming trust,
/// which is how this class of bug ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// The engine's own `Cbuf_AddText`: startup, `valve.rc`, `stuffcmds`.
    #[default]
    Code,
    /// A keybind, and later the console input line.
    UserInput,
    /// `ClientCmd`. Arrives with `client/`.
    ClientCmd,
    /// `clc_stringcmd` from a connected client. Arrives with `net/`.
    NetClient,
    /// From the server we are connected to. Arrives with `net/`.
    NetServer,
    /// Played back from a `.dem`. Arrives with `demo/`.
    DemoFile,
}

impl Source {
    /// Whether the source is one this port can currently produce.
    ///
    /// The unreachable variants are not dead weight — see the type's docs — but
    /// a dispatcher that meets one today has been handed something it has no
    /// way to have received, and should say so rather than trust it.
    pub fn is_trusted_local(self) -> bool {
        matches!(self, Source::Code | Source::UserInput)
    }
}

/// The break set: `{}()':`.
///
/// `CharacterSetBuild( &s_BreakSet, "{}()':" )` (`tier1/convar.cpp:341`). Each
/// of these is its own single-character token wherever it appears, so
/// `bind ' +attack` gives three arguments and not two.
const BREAK_SET: &[u8] = b"{}()':";

fn is_break(c: u8) -> bool {
    BREAK_SET.contains(&c)
}

/// One dequeued command: the name, its arguments, and the raw tail.
///
/// **Owned, where Valve's `CCommand` points into the command buffer.** That is
/// forced rather than chosen, and Valve hit the same problem from the other
/// side: the comment at `tier1/convar.cpp:421` explains its `memcpy` as being
/// "here to avoid the pointers returned by `DequeueNextCommand` to become
/// invalid by calling `AddText`". Dispatching a command can insert more text
/// (an alias expands, an `exec` runs a line), so the borrow could not survive
/// dispatch even if the C++ pretended it did. `ENGINE_CONSOLE.md` §6.4 sketches
/// this as `Command<'a>`; owning it is the correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    argv: Vec<String>,
    tail: String,
    source: Source,
}

impl Command {
    /// Splits one command's text into argv.
    ///
    /// Never fails: a command that tokenizes to nothing is an empty `Command`,
    /// which the dispatcher skips. Valve returns `false` here only for
    /// overflow of its fixed 512-byte buffers, which do not exist.
    pub fn parse(text: &str, source: Source) -> Command {
        let (argv, argv0_size) = tokenize(text);
        // `CCommand::ArgS` (`public/tier1/convar.h:321`) returns "" when
        // `m_nArgv0Size` is zero, which is exactly the fewer-than-two-tokens
        // case, so this matches rather than diverging.
        let tail = match argv.len() >= 2 {
            true => text[argv0_size.min(text.len())..].to_string(),
            false => String::new(),
        };
        Command { argv, tail, source }
    }

    /// argv[0]: the command or cvar name. `""` for an empty command.
    pub fn name(&self) -> &str {
        self.argv.first().map_or("", String::as_str)
    }

    /// Everything after argv[0].
    pub fn args(&self) -> &[String] {
        self.argv.get(1..).unwrap_or(&[])
    }

    /// `CCommand::operator[]`, counting argv[0] as 0.
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.argv.get(index).map(String::as_str)
    }

    /// `CCommand::ArgC`.
    pub fn argc(&self) -> usize {
        self.argv.len()
    }

    pub fn is_empty(&self) -> bool {
        self.argv.is_empty()
    }

    /// `CCommand::ArgS`: everything after argv[0], **as typed**.
    ///
    /// This is not `args().join(" ")` and the difference is the point. The
    /// tokenizer strips quotes; the tail does not, so `hostname "  a b  "`
    /// still has its interior spaces here and the cvar set path
    /// ([`super::strip_set_value`]) is what removes the surrounding quotes.
    /// Reconstructing this from the tokens would lose exactly the information
    /// it exists to carry.
    pub fn tail(&self) -> &str {
        &self.tail
    }

    pub fn source(&self) -> Source {
        self.source
    }
}

/// argv, plus the offset just past argv[0] that [`Command::tail`] starts at.
///
/// The offset is Valve's `m_nArgv0Size` and its arithmetic
/// (`tier1/convar.cpp:451-471`) is reproduced rather than reinvented, because
/// it is doing something non-obvious: it is computed **after parsing the second
/// token**, by taking that token's start position and then backing over the
/// quote that opened it, if there was one. That is what makes `"foo"bar` give
/// two arguments with a tail of `bar`, and what makes a quoted argument's tail
/// keep its opening quote.
fn tokenize(text: &str) -> (Vec<String>, usize) {
    let bytes = text.as_bytes();
    let mut argv: Vec<String> = Vec::new();
    let mut pos = 0;
    let mut argv0_size = 0;

    loop {
        let start_get = pos;
        let Some((token, size)) = parse_token(text, &mut pos) else {
            break;
        };

        if argv.len() == 1 {
            // `m_nArgv0Size = bufParse.TellGet()`, then walked back.
            let mut end = pos;
            if end > 0 && bytes[end - 1] == b'"' {
                end -= 1;
            }
            end = end.saturating_sub(size);
            if end > start_get && end > 0 && bytes[end - 1] == b'"' {
                end -= 1;
            }
            argv0_size = end;
        }

        argv.push(token);
    }

    (argv, argv0_size)
}

/// `CUtlBuffer::ParseToken`. Returns the token and the length Valve reports for
/// it, which is the *content* length — quotes are not counted.
fn parse_token(text: &str, pos: &mut usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();

    // `EatWhiteSpace` then `EatCPPComment`, repeatedly: a comment can be
    // followed by more whitespace and another comment.
    loop {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if *pos + 1 < bytes.len() && bytes[*pos] == b'/' && bytes[*pos + 1] == b'/' {
            *pos += 2;
            while *pos < bytes.len() && bytes[*pos] != b'\n' {
                *pos += 1;
            }
            continue;
        }
        break;
    }

    if *pos >= bytes.len() {
        return None;
    }

    let c = bytes[*pos];
    *pos += 1;

    // Quoted: one token, quotes stripped, and an unterminated quote runs to the
    // end of the command rather than being an error.
    if c == b'"' {
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos] != b'"' {
            *pos += 1;
        }
        let token = &text[start..*pos];
        if *pos < bytes.len() {
            *pos += 1;
        }
        return Some((token.to_string(), token.len()));
    }

    // A break character is a token on its own.
    if is_break(c) {
        return Some(((c as char).to_string(), 1));
    }

    // A bare word, ended by a break character, a quote, or anything at or below
    // a space. Note that the quote ends the word *without* being consumed, so
    // `foo"bar"` is two tokens.
    let start = *pos - 1;
    while *pos < bytes.len() {
        let next = bytes[*pos];
        if is_break(next) || next == b'"' || next <= b' ' {
            break;
        }
        *pos += 1;
    }
    let token = &text[start..*pos];
    Some((token.to_string(), token.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(text: &str) -> Vec<String> {
        Command::parse(text, Source::Code).argv
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(argv("map sp_a1_intro1"), ["map", "sp_a1_intro1"]);
        assert_eq!(argv("  echo\t hello \n"), ["echo", "hello"]);
        assert!(argv("").is_empty());
        assert!(argv("   ").is_empty());
    }

    #[test]
    fn a_quoted_argument_is_one_token_without_its_quotes() {
        let cmd = Command::parse(r#"hostname "a b c""#, Source::Code);
        assert_eq!(cmd.name(), "hostname");
        assert_eq!(cmd.args(), ["a b c"]);
    }

    #[test]
    fn an_unterminated_quote_runs_to_the_end() {
        assert_eq!(argv(r#"say "hello there"#), ["say", "hello there"]);
    }

    #[test]
    fn break_characters_are_their_own_tokens() {
        // `{}()':` -- so an alias body in braces tokenizes without the braces
        // gluing themselves to the words either side.
        assert_eq!(argv("alias x {y}"), ["alias", "x", "{", "y", "}"]);
        assert_eq!(argv("bind ' +attack"), ["bind", "'", "+attack"]);
    }

    #[test]
    fn comments_are_skipped_wherever_they_start() {
        assert_eq!(
            argv("fps_max 120 // cap the frame rate"),
            ["fps_max", "120"]
        );
        assert_eq!(argv("// nothing but a comment"), Vec::<String>::new());
        assert_eq!(argv("echo // a\n hi"), ["echo", "hi"]);
    }

    #[test]
    fn a_lone_slash_is_not_a_comment() {
        assert_eq!(argv("exec cfg/foo"), ["exec", "cfg/foo"]);
    }

    #[test]
    fn tail_is_the_raw_remainder_not_the_rejoined_tokens() {
        // The whole reason `ArgS` exists: the set path needs the text as typed,
        // because the tokenizer has already thrown the quotes away.
        let cmd = Command::parse(r#"hostname "  a b  ""#, Source::Code);
        assert_eq!(cmd.args(), ["  a b  "]);
        assert_eq!(cmd.tail(), r#""  a b  ""#, "quotes survive in the tail");

        let cmd = Command::parse("echo hello   world", Source::Code);
        assert_eq!(cmd.tail(), "hello   world", "interior spacing is preserved");
    }

    /// `tier1/convar.cpp:466` calls this case out by name: "The StartGet check
    /// is to handle this case: `\"foo\"bar` which will parse into 2 different
    /// args. ArgS should point to bar."
    #[test]
    fn quoted_argv0_followed_immediately_by_a_word() {
        let cmd = Command::parse(r#""foo"bar"#, Source::Code);
        assert_eq!(cmd.name(), "foo");
        assert_eq!(cmd.args(), ["bar"]);
        assert_eq!(cmd.tail(), "bar");
    }

    #[test]
    fn tail_is_empty_below_two_tokens() {
        assert_eq!(Command::parse("quit", Source::Code).tail(), "");
        assert_eq!(Command::parse("", Source::Code).tail(), "");
    }

    #[test]
    fn a_semicolon_is_an_ordinary_character_here() {
        // Splitting on `;` is the *other* tokenizer's job, and it has already
        // run by the time this one sees the text.
        assert_eq!(argv("echo a;b"), ["echo", "a;b"]);
    }

    #[test]
    fn source_is_carried_through() {
        let cmd = Command::parse("+forward 0", Source::UserInput);
        assert_eq!(cmd.source(), Source::UserInput);
        assert!(cmd.source().is_trusted_local());
        assert!(!Source::NetClient.is_trusted_local());
    }
}
