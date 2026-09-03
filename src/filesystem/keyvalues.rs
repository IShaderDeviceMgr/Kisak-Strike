//! A reader for Valve's KeyValues text format.
//!
//! Ported from `tier1/KeyValues.cpp`'s `RecursiveLoadFromBuffer` and
//! `tier1/exprevaluator.cpp`. Only the *reader* is here, and only the text
//! form — the binary pooled format (`KV_BINARY_POOLED_FORMAT`) is a console
//! load-time optimization and `KeyValuesPreloadType_t` is deleted outright per
//! `portdocs/FILESYSTEM.md`'s disposition table.
//!
//! The format is fixed: Valve authors `gameinfo.txt`, `.vmt`, soundscapes and
//! more, so this parses what they emit rather than anything we would design.
//! The *data model* is ours — an ordered `Vec` of entries instead of an
//! intrusive linked list of refcounted `KeyValues` nodes with `deleteThis()`.
//!
//! Two behaviours here are easy to get wrong and are load-bearing:
//!
//! * **Escape sequences are off.** Valve's `LoadFromBuffer` only honours `\n`,
//!   `\t`, `\\` and `\"` when `UsesEscapeSequences(true)` has been called, and
//!   nothing does that for `gameinfo.txt`. Since Valve content is full of
//!   Windows-authored paths, treating `\` as an escape would corrupt them.
//! * **`$WIN32` does not mean Windows.** See [`ConditionalSymbols`].

use crate::filesystem::error::{Result, VfsError};

/// A parsed value: either a leaf string or a nested block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(String),
    Block(Block),
}

/// One `key value` or `key { ... }` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
}

/// An ordered list of entries.
///
/// Duplicate keys are preserved in source order — `gameinfo.txt`'s
/// `SearchPaths` block repeats the `Game` key and the order *is* the search
/// order, so deduplicating into a map would lose the file's meaning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    entries: Vec<Entry>,
}

impl Block {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// First entry whose key matches case-insensitively — `KeyValues::FindKey`.
    pub fn find(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|e| e.key.eq_ignore_ascii_case(key))
            .map(|e| &e.value)
    }

    /// First matching entry, if it is a block.
    pub fn find_block(&self, key: &str) -> Option<&Block> {
        match self.find(key) {
            Some(Value::Block(b)) => Some(b),
            _ => None,
        }
    }

    /// First matching entry, if it is a string.
    pub fn find_string(&self, key: &str) -> Option<&str> {
        match self.find(key) {
            Some(Value::String(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Leaf key/value pairs in source order, skipping nested blocks.
    ///
    /// This is `GetFirstValue`/`GetNextValue`, which is what
    /// `FileSystem_LoadSearchPaths` iterates (`filesystem_init.cpp:746`) — so
    /// a stray sub-block inside `SearchPaths` is skipped, not treated as a path.
    pub fn values(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|e| match &e.value {
            Value::String(s) => Some((e.key.as_str(), s.as_str())),
            Value::Block(_) => None,
        })
    }

    /// The first entry whose value is a block.
    ///
    /// `ReadKeyValuesFile` (`filesystem_init.cpp:318`) parses into a nameless
    /// root and then reads through it, so the outermost `"GameInfo"` wrapper is
    /// located positionally, not by name. Mods do rename it.
    pub fn first_block(&self) -> Option<&Block> {
        self.entries.iter().find_map(|e| match &e.value {
            Value::Block(b) => Some(b),
            Value::String(_) => None,
        })
    }
}

/// Which `$SYMBOL`s in `[...]` conditionals evaluate true.
///
/// Ported from `DefaultConditionalSymbolProc` (`tier1/exprevaluator.cpp:24`).
///
/// **`$WIN32` is `IsPC()`, not "is Windows".** In Valve's platform vocabulary
/// "PC" means "not a game console", so `[$WIN32]` is *true* on Linux and macOS
/// and the sections it guards must be kept. Reading it as `cfg!(windows)` would
/// silently drop search paths from `gameinfo.txt` — the failure would surface
/// much later as missing content, with nothing pointing back here.
/// `$WINDOWS` is the symbol that actually means Windows, and that one is false.
#[derive(Debug, Clone, Copy)]
pub struct ConditionalSymbols;

impl ConditionalSymbols {
    /// Resolves one symbol. `name` may or may not carry its leading `$`.
    pub fn get(name: &str) -> bool {
        let name = name.strip_prefix('$').unwrap_or(name);

        // POSIX only, never a console — see PORTING.md, "Supported platforms".
        if name.eq_ignore_ascii_case("WIN32") {
            return true; // IsPC()
        }
        if name.eq_ignore_ascii_case("WINDOWS") {
            return false; // IsPlatformWindowsPC()
        }
        if name.eq_ignore_ascii_case("POSIX") {
            return true;
        }
        if name.eq_ignore_ascii_case("OSX") {
            return cfg!(target_os = "macos");
        }
        if name.eq_ignore_ascii_case("LINUX") {
            return cfg!(target_os = "linux");
        }
        // X360, PS3, GAMECONSOLE, DEMO, LOWVIOLENCE, and any run-time symbol
        // registered through KeyValuesSystem (we register none).
        false
    }
}

/// Evaluates a conditional expression: `$A`, `!$A`, `$A && $B`, `$A || $B`,
/// parentheses, and the constants `0`/`1`.
///
/// `CExpressionEvaluator` (`tier1/exprevaluator.cpp`) written as recursive
/// descent. Valve's version reports a syntax error and yields false; so does
/// this one, which is why it returns a plain `bool`.
fn evaluate_conditional(expr: &str) -> bool {
    Cond {
        bytes: expr.as_bytes(),
        pos: 0,
    }
    .parse_or()
    .unwrap_or(false)
}

struct Cond<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cond<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, want: &[u8]) -> bool {
        self.skip_ws();
        if self.bytes[self.pos..].starts_with(want) {
            self.pos += want.len();
            true
        } else {
            false
        }
    }

    fn parse_or(&mut self) -> Option<bool> {
        let mut acc = self.parse_and()?;
        while self.eat(b"||") {
            // No short-circuit: the right side must still parse for the whole
            // expression to be considered valid, matching Valve's tree build.
            acc = self.parse_and()? || acc;
        }
        Some(acc)
    }

    fn parse_and(&mut self) -> Option<bool> {
        let mut acc = self.parse_unary()?;
        while self.eat(b"&&") {
            acc = self.parse_unary()? && acc;
        }
        Some(acc)
    }

    fn parse_unary(&mut self) -> Option<bool> {
        if self.eat(b"!") {
            return Some(!self.parse_unary()?);
        }
        if self.eat(b"(") {
            let inner = self.parse_or()?;
            if !self.eat(b")") {
                return None;
            }
            return Some(inner);
        }

        match self.peek()? {
            b'$' => {
                let start = self.pos;
                self.pos += 1; // the '$'
                while self.pos < self.bytes.len()
                    && (self.bytes[self.pos].is_ascii_alphanumeric()
                        || self.bytes[self.pos] == b'_')
                {
                    self.pos += 1;
                }
                let ident = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
                Some(ConditionalSymbols::get(ident))
            }
            b'0' => {
                self.pos += 1;
                Some(false)
            }
            b'1' => {
                self.pos += 1;
                Some(true)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// A quoted or bare string.
    Str(String),
    OpenBrace,
    CloseBrace,
    /// The contents of a `[...]` conditional, without the brackets.
    Conditional(String),
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    name: &'a str,
}

impl<'a> Lexer<'a> {
    fn new(name: &'a str, src: &'a str) -> Self {
        Lexer {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            name,
        }
    }

    fn err(&self, reason: impl Into<String>) -> VfsError {
        VfsError::KeyValues {
            source_name: self.name.to_string(),
            line: self.line,
            reason: reason.into(),
        }
    }

    /// Skips whitespace and `//` comments. Valve's tokenizer has no block
    /// comment form, so neither does this.
    fn skip_trivia(&mut self) {
        loop {
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
                if self.src[self.pos] == b'\n' {
                    self.line += 1;
                }
                self.pos += 1;
            }
            if self.src[self.pos..].starts_with(b"//") {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>> {
        self.skip_trivia();
        let Some(&c) = self.src.get(self.pos) else {
            return Ok(None);
        };

        match c {
            b'{' => {
                self.pos += 1;
                Ok(Some(Token::OpenBrace))
            }
            b'}' => {
                self.pos += 1;
                Ok(Some(Token::CloseBrace))
            }
            b'[' => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.src.len() && self.src[self.pos] != b']' {
                    if self.src[self.pos] == b'\n' {
                        return Err(self.err("unterminated [conditional]"));
                    }
                    self.pos += 1;
                }
                if self.pos >= self.src.len() {
                    return Err(self.err("unterminated [conditional]"));
                }
                let text = self.slice(start, self.pos);
                self.pos += 1; // the ']'
                Ok(Some(Token::Conditional(text)))
            }
            b'"' => {
                self.pos += 1;
                let start = self.pos;
                // No escape processing: see the module docs.
                while self.pos < self.src.len() && self.src[self.pos] != b'"' {
                    if self.src[self.pos] == b'\n' {
                        self.line += 1;
                    }
                    self.pos += 1;
                }
                if self.pos >= self.src.len() {
                    return Err(self.err("unterminated quoted string"));
                }
                let text = self.slice(start, self.pos);
                self.pos += 1; // the closing quote
                Ok(Some(Token::Str(text)))
            }
            _ => {
                let start = self.pos;
                while self.pos < self.src.len() {
                    let b = self.src[self.pos];
                    if b.is_ascii_whitespace() || matches!(b, b'{' | b'}' | b'"' | b'[' | b']') {
                        break;
                    }
                    if self.src[self.pos..].starts_with(b"//") {
                        break;
                    }
                    self.pos += 1;
                }
                Ok(Some(Token::Str(self.slice(start, self.pos))))
            }
        }
    }

    /// Valve content is predominantly ASCII but localized files are not always
    /// valid UTF-8. Lossy conversion keeps a stray byte from failing an entire
    /// gameinfo parse.
    fn slice(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.src[start..end]).into_owned()
    }
}

/// Parses a KeyValues document.
///
/// `source_name` is used only in error messages.
pub fn parse(source_name: &str, text: &str) -> Result<Block> {
    let mut lexer = Lexer::new(source_name, text);
    let block = parse_block(&mut lexer, true)?;
    Ok(block)
}

fn parse_block(lexer: &mut Lexer<'_>, top_level: bool) -> Result<Block> {
    let mut entries = Vec::new();

    loop {
        let Some(token) = lexer.next_token()? else {
            if top_level {
                return Ok(Block { entries });
            }
            return Err(lexer.err("unexpected end of file inside a block"));
        };

        let key = match token {
            Token::CloseBrace if !top_level => return Ok(Block { entries }),
            Token::CloseBrace => return Err(lexer.err("unmatched '}'")),
            Token::Str(s) => s,
            Token::OpenBrace => return Err(lexer.err("expected a key, found '{'")),
            Token::Conditional(_) => return Err(lexer.err("expected a key, found a [conditional]")),
        };

        // Valve accepts a conditional either between the key and the value or
        // after the value; both gate the same pair. Collect whatever appears.
        let mut accepted = true;
        let mut token = lexer
            .next_token()?
            .ok_or_else(|| lexer.err(format!("key {key:?} has no value")))?;

        if let Token::Conditional(expr) = token {
            accepted &= evaluate_conditional(&expr);
            token = lexer
                .next_token()?
                .ok_or_else(|| lexer.err(format!("key {key:?} has no value")))?;
        }

        let value = match token {
            Token::OpenBrace => Value::Block(parse_block(lexer, false)?),
            Token::Str(s) => Value::String(s),
            Token::CloseBrace => {
                return Err(lexer.err(format!("key {key:?} has no value")));
            }
            Token::Conditional(_) => {
                return Err(lexer.err(format!("key {key:?} has two conditionals and no value")));
            }
        };

        // A trailing conditional, if any. Peek without consuming anything else.
        let save_pos = lexer.pos;
        let save_line = lexer.line;
        match lexer.next_token()? {
            Some(Token::Conditional(expr)) => accepted &= evaluate_conditional(&expr),
            _ => {
                lexer.pos = save_pos;
                lexer.line = save_line;
            }
        }

        if accepted {
            entries.push(Entry { key, value });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_gameinfo_shaped_document() {
        let src = r#"
"GameInfo"
{
	game 	"Portal 2"
	title	"Portal 2"

	FileSystem
	{
		SteamAppId				620

		SearchPaths
		{
			Game				|gameinfo_path|.
			Game				|all_source_engine_paths|portal2_dlc2
			Game				|all_source_engine_paths|portal2
		}
	}
}
"#;
        let doc = parse("gameinfo.txt", src).unwrap();
        let root = doc.first_block().unwrap();
        assert_eq!(root.find_string("game"), Some("Portal 2"));

        let fs = root.find_block("FileSystem").unwrap();
        assert_eq!(fs.find_string("steamappid"), Some("620"));

        let paths: Vec<_> = fs.find_block("SearchPaths").unwrap().values().collect();
        assert_eq!(
            paths,
            vec![
                ("Game", "|gameinfo_path|."),
                ("Game", "|all_source_engine_paths|portal2_dlc2"),
                ("Game", "|all_source_engine_paths|portal2"),
            ]
        );
    }

    #[test]
    fn win32_conditional_is_kept_on_posix() {
        // The trap: $WIN32 is IsPC(), so this path must survive on Linux/macOS.
        let src = r#"
"GameInfo" { FileSystem { SearchPaths {
	Game	somewhere            [$WIN32]
	Game	console_only         [$X360]
	Game	windows_only         [$WINDOWS]
	Game	posix_only           [$POSIX]
	Game	not_console          [!$GAMECONSOLE]
} } }
"#;
        let doc = parse("gameinfo.txt", src).unwrap();
        let paths: Vec<_> = doc
            .first_block()
            .unwrap()
            .find_block("FileSystem")
            .unwrap()
            .find_block("SearchPaths")
            .unwrap()
            .values()
            .map(|(_, v)| v)
            .collect();
        assert_eq!(paths, vec!["somewhere", "posix_only", "not_console"]);
    }

    #[test]
    fn conditional_expressions() {
        assert!(evaluate_conditional("$WIN32"));
        assert!(!evaluate_conditional("$X360"));
        assert!(evaluate_conditional("!$X360"));
        assert!(evaluate_conditional("$WIN32 && !$PS3"));
        assert!(evaluate_conditional("$X360 || $WIN32"));
        assert!(!evaluate_conditional("$X360 || $PS3"));
        assert!(evaluate_conditional("($X360 || $WIN32) && !$WINDOWS"));
        assert!(evaluate_conditional("1"));
        assert!(!evaluate_conditional("0"));
        // Malformed expressions evaluate false rather than aborting the parse.
        assert!(!evaluate_conditional("$WIN32 &&"));
        assert!(!evaluate_conditional(""));
    }

    #[test]
    fn os_specific_symbols_agree_with_the_target() {
        assert_eq!(evaluate_conditional("$LINUX"), cfg!(target_os = "linux"));
        assert_eq!(evaluate_conditional("$OSX"), cfg!(target_os = "macos"));
    }

    #[test]
    fn backslashes_are_literal_not_escapes() {
        // Windows-authored content paths must survive verbatim.
        let doc = parse("t", r#""root" { "path" "materials\metal\wall.vmt" }"#).unwrap();
        assert_eq!(
            doc.first_block().unwrap().find_string("path"),
            Some(r"materials\metal\wall.vmt")
        );
    }

    #[test]
    fn comments_and_bare_tokens() {
        let src = r#"
// leading comment
"root"
{
	bare_key	bare_value	// trailing comment
	"quoted key"	"quoted value"
}
"#;
        let doc = parse("t", src).unwrap();
        let root = doc.first_block().unwrap();
        assert_eq!(root.find_string("bare_key"), Some("bare_value"));
        assert_eq!(root.find_string("quoted key"), Some("quoted value"));
    }

    #[test]
    fn duplicate_keys_are_preserved_in_order() {
        let doc = parse("t", r#""r" { a 1  a 2  a 3 }"#).unwrap();
        let root = doc.first_block().unwrap();
        let vals: Vec<_> = root.values().map(|(_, v)| v).collect();
        assert_eq!(vals, vec!["1", "2", "3"]);
        // FindKey semantics: first match wins.
        assert_eq!(root.find_string("a"), Some("1"));
    }

    #[test]
    fn nested_blocks_are_skipped_by_values() {
        let doc = parse("t", r#""r" { a 1  sub { b 2 }  c 3 }"#).unwrap();
        let root = doc.first_block().unwrap();
        let vals: Vec<_> = root.values().collect();
        assert_eq!(vals, vec![("a", "1"), ("c", "3")]);
        assert!(root.find_block("sub").is_some());
    }

    #[test]
    fn key_lookup_is_case_insensitive() {
        let doc = parse("t", r#""r" { SteamAppId 620 }"#).unwrap();
        let root = doc.first_block().unwrap();
        assert_eq!(root.find_string("STEAMAPPID"), Some("620"));
        assert_eq!(root.find_string("steamappid"), Some("620"));
    }

    #[test]
    fn reports_malformed_input() {
        assert!(parse("t", r#""r" { a "#).is_err());
        assert!(parse("t", r#""r" { "unterminated }"#).is_err());
        assert!(parse("t", "}").is_err());
        assert!(parse("t", r#""r" { a [$WIN32 }"#).is_err());
    }

    #[test]
    fn empty_document_is_not_an_error() {
        let doc = parse("t", "  // nothing here\n").unwrap();
        assert!(doc.is_empty());
        assert!(doc.first_block().is_none());
    }
}
