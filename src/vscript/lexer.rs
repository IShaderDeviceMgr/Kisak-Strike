//! `SQLexer` (`sqlexer.cpp`).
//!
//! Tokens are Squirrel's, including the one piece of lexer state the grammar
//! reads directly: **whether a newline came before the current token**
//! (`_prevtoken == '\n'`). A newline ends a statement, stops a `[` from
//! indexing the line above it, and keeps a `++` on the next line from being a
//! postfix increment — so the parser asks the lexer, exactly as
//! `SQCompiler::IsEndOfStatement` does.

use super::value::SqStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    /// A single-character token: `{ } ( ) [ ] ; , ? ^ ~ . < > = ! & | : * / % + -`
    /// and anything else the lexer passes through.
    Char(u8),
    Identifier,
    StringLiteral,
    Integer,
    Float,
    // Keywords.
    While,
    Do,
    If,
    Else,
    Break,
    Continue,
    Return,
    Null,
    Function,
    Local,
    For,
    Foreach,
    In,
    Typeof,
    Delegate,
    Delete,
    Try,
    Catch,
    Throw,
    Clone,
    Yield,
    Resume,
    Switch,
    Case,
    Default,
    This,
    Parent,
    Class,
    Extends,
    Constructor,
    Instanceof,
    Vargc,
    Vargv,
    True,
    False,
    Static,
    Enum,
    Const,
    // Operators longer than one character.
    Eq,
    Ne,
    Le,
    Ge,
    And,
    Or,
    NewSlot,
    PlusEq,
    MinusEq,
    MulEq,
    DivEq,
    ModEq,
    PlusPlus,
    MinusMinus,
    ShiftL,
    ShiftR,
    UShiftR,
    DoubleColon,
    VarParams,
    AttrOpen,
    AttrClose,
    /// End of input — `SQUIRREL_EOB`.
    Eob,
}

impl Tok {
    /// `Tok2Str` and the `Expect` error's naming of a token.
    pub fn describe(self) -> String {
        match self {
            Tok::Char(c) => (c as char).to_string(),
            Tok::Identifier => "IDENTIFIER".into(),
            Tok::StringLiteral => "STRING_LITERAL".into(),
            Tok::Integer => "INTEGER".into(),
            Tok::Float => "FLOAT".into(),
            other => KEYWORDS
                .iter()
                .find(|(_, t)| *t == other)
                .map(|(name, _)| (*name).to_owned())
                .unwrap_or_else(|| format!("{other:?}")),
        }
    }
}

const KEYWORDS: &[(&str, Tok)] = &[
    ("while", Tok::While),
    ("do", Tok::Do),
    ("if", Tok::If),
    ("else", Tok::Else),
    ("break", Tok::Break),
    ("continue", Tok::Continue),
    ("return", Tok::Return),
    ("null", Tok::Null),
    ("function", Tok::Function),
    ("local", Tok::Local),
    ("for", Tok::For),
    ("foreach", Tok::Foreach),
    ("in", Tok::In),
    ("typeof", Tok::Typeof),
    ("delegate", Tok::Delegate),
    ("delete", Tok::Delete),
    ("try", Tok::Try),
    ("catch", Tok::Catch),
    ("throw", Tok::Throw),
    ("clone", Tok::Clone),
    ("yield", Tok::Yield),
    ("resume", Tok::Resume),
    ("switch", Tok::Switch),
    ("case", Tok::Case),
    ("default", Tok::Default),
    ("this", Tok::This),
    ("parent", Tok::Parent),
    ("class", Tok::Class),
    ("extends", Tok::Extends),
    ("constructor", Tok::Constructor),
    ("instanceof", Tok::Instanceof),
    ("vargc", Tok::Vargc),
    ("vargv", Tok::Vargv),
    ("true", Tok::True),
    ("false", Tok::False),
    ("static", Tok::Static),
    ("enum", Tok::Enum),
    ("const", Tok::Const),
];

/// A compile error: `SQCompiler::Error`'s text and where the lexer was.
#[derive(Clone, Debug)]
pub struct CompileError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

/// The lexer's view of what came before the current token. `Newline` is the
/// `'\n'` marker `_prevtoken` can hold; `None` is the `-1` it starts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prev {
    None,
    Newline,
    Token(Tok),
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    /// `_currdata`, or `None` at the end.
    cur: Option<u8>,
    cur_token: Prev,
    pub prev_token: Prev,
    pub current_line: u32,
    pub last_token_line: u32,
    pub current_column: u32,
    pub svalue: SqStr,
    pub nvalue: i32,
    pub fvalue: f32,
}

type LexResult<T> = Result<T, CompileError>;

fn is_alpha(c: u8) -> bool {
    c.is_ascii_alphabetic()
}

fn is_octal(c: u8) -> bool {
    (b'0'..=b'7').contains(&c)
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8]) -> Lexer<'a> {
        let mut lexer = Lexer {
            src,
            pos: 0,
            cur: None,
            cur_token: Prev::None,
            prev_token: Prev::None,
            current_line: 1,
            last_token_line: 1,
            current_column: 0,
            svalue: SqStr::new(b""),
            nvalue: 0,
            fvalue: 0.0,
        };
        lexer.read();
        lexer
    }

    /// `SQLexer::Next` — the byte reader treats a NUL as the end, as the
    /// buffer reader in `sq_compilebuffer` does.
    fn read(&mut self) {
        self.cur = match self.src.get(self.pos) {
            Some(&0) | None => None,
            Some(&c) => Some(c),
        };
        self.pos += 1;
    }

    fn next(&mut self) {
        self.read();
        self.current_column += 1;
    }

    fn error(&self, message: &str) -> CompileError {
        CompileError {
            message: message.to_owned(),
            line: self.current_line,
            column: self.current_column,
        }
    }

    fn ret(&mut self, t: Tok) -> LexResult<Tok> {
        self.prev_token = self.cur_token;
        self.cur_token = Prev::Token(t);
        Ok(t)
    }

    /// `SQLexer::Lex`.
    pub fn lex(&mut self) -> LexResult<Tok> {
        self.last_token_line = self.current_line;
        while let Some(c) = self.cur {
            match c {
                b'\t' | b'\r' | b' ' => {
                    self.next();
                    continue;
                }
                b'\n' => {
                    self.current_line += 1;
                    self.prev_token = self.cur_token;
                    self.cur_token = Prev::Newline;
                    self.next();
                    self.current_column = 1;
                    continue;
                }
                b'/' => {
                    self.next();
                    match self.cur {
                        Some(b'*') => {
                            self.next();
                            self.block_comment()?;
                            continue;
                        }
                        Some(b'/') => {
                            loop {
                                self.next();
                                if self.cur.is_none() || self.cur == Some(b'\n') {
                                    break;
                                }
                            }
                            continue;
                        }
                        Some(b'=') => {
                            self.next();
                            return self.ret(Tok::DivEq);
                        }
                        Some(b'>') => {
                            self.next();
                            return self.ret(Tok::AttrClose);
                        }
                        _ => return self.ret(Tok::Char(b'/')),
                    }
                }
                b'=' => {
                    self.next();
                    if self.cur != Some(b'=') {
                        return self.ret(Tok::Char(b'='));
                    }
                    self.next();
                    return self.ret(Tok::Eq);
                }
                b'<' => {
                    self.next();
                    return match self.cur {
                        Some(b'=') => {
                            self.next();
                            self.ret(Tok::Le)
                        }
                        Some(b'-') => {
                            self.next();
                            self.ret(Tok::NewSlot)
                        }
                        Some(b'<') => {
                            self.next();
                            self.ret(Tok::ShiftL)
                        }
                        Some(b'/') => {
                            self.next();
                            self.ret(Tok::AttrOpen)
                        }
                        _ => self.ret(Tok::Char(b'<')),
                    };
                }
                b'>' => {
                    self.next();
                    if self.cur == Some(b'=') {
                        self.next();
                        return self.ret(Tok::Ge);
                    }
                    if self.cur == Some(b'>') {
                        self.next();
                        if self.cur == Some(b'>') {
                            self.next();
                            return self.ret(Tok::UShiftR);
                        }
                        return self.ret(Tok::ShiftR);
                    }
                    return self.ret(Tok::Char(b'>'));
                }
                b'!' => {
                    self.next();
                    if self.cur != Some(b'=') {
                        return self.ret(Tok::Char(b'!'));
                    }
                    self.next();
                    return self.ret(Tok::Ne);
                }
                b'@' => {
                    self.next();
                    if self.cur != Some(b'"') {
                        return Err(self.error("string expected"));
                    }
                    let t = self.read_string(b'"', true)?;
                    return self.ret(t);
                }
                b'"' | b'\'' => {
                    let t = self.read_string(c, false)?;
                    return self.ret(t);
                }
                b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';' | b',' | b'?' | b'^' | b'~' => {
                    self.next();
                    return self.ret(Tok::Char(c));
                }
                b'.' => {
                    self.next();
                    if self.cur != Some(b'.') {
                        return self.ret(Tok::Char(b'.'));
                    }
                    self.next();
                    if self.cur != Some(b'.') {
                        return Err(self.error("invalid token '..'"));
                    }
                    self.next();
                    return self.ret(Tok::VarParams);
                }
                b'&' => {
                    self.next();
                    if self.cur != Some(b'&') {
                        return self.ret(Tok::Char(b'&'));
                    }
                    self.next();
                    return self.ret(Tok::And);
                }
                b'|' => {
                    self.next();
                    if self.cur != Some(b'|') {
                        return self.ret(Tok::Char(b'|'));
                    }
                    self.next();
                    return self.ret(Tok::Or);
                }
                b':' => {
                    self.next();
                    if self.cur != Some(b':') {
                        return self.ret(Tok::Char(b':'));
                    }
                    self.next();
                    return self.ret(Tok::DoubleColon);
                }
                b'*' => {
                    self.next();
                    if self.cur == Some(b'=') {
                        self.next();
                        return self.ret(Tok::MulEq);
                    }
                    return self.ret(Tok::Char(b'*'));
                }
                b'%' => {
                    self.next();
                    if self.cur == Some(b'=') {
                        self.next();
                        return self.ret(Tok::ModEq);
                    }
                    return self.ret(Tok::Char(b'%'));
                }
                b'-' => {
                    self.next();
                    if self.cur == Some(b'=') {
                        self.next();
                        return self.ret(Tok::MinusEq);
                    }
                    if self.cur == Some(b'-') {
                        self.next();
                        return self.ret(Tok::MinusMinus);
                    }
                    return self.ret(Tok::Char(b'-'));
                }
                b'+' => {
                    self.next();
                    if self.cur == Some(b'=') {
                        self.next();
                        return self.ret(Tok::PlusEq);
                    }
                    if self.cur == Some(b'+') {
                        self.next();
                        return self.ret(Tok::PlusPlus);
                    }
                    return self.ret(Tok::Char(b'+'));
                }
                _ => {
                    if c.is_ascii_digit() {
                        let t = self.read_number()?;
                        return self.ret(t);
                    }
                    if is_alpha(c) || c == b'_' {
                        let t = self.read_id();
                        return self.ret(t);
                    }
                    if c.is_ascii_control() {
                        return Err(self.error("unexpected character(control)"));
                    }
                    self.next();
                    return self.ret(Tok::Char(c));
                }
            }
        }
        Ok(Tok::Eob)
    }

    fn block_comment(&mut self) -> LexResult<()> {
        loop {
            match self.cur {
                Some(b'*') => {
                    self.next();
                    if self.cur == Some(b'/') {
                        self.next();
                        return Ok(());
                    }
                }
                Some(b'\n') => {
                    self.current_line += 1;
                    self.next();
                }
                None => return Err(self.error("missing \"*/\" in comment")),
                _ => self.next(),
            }
        }
    }

    /// `SQLexer::ReadString`. A single-quoted literal is a *character*: one
    /// byte, returned as an integer — sign-extended, because `SQChar` is a
    /// signed `char`.
    fn read_string(&mut self, delim: u8, verbatim: bool) -> LexResult<Tok> {
        let mut buf: Vec<u8> = Vec::new();
        self.next();
        if self.cur.is_none() {
            return Err(self.error("error parsing the string"));
        }
        loop {
            while self.cur != Some(delim) {
                match self.cur {
                    None => return Err(self.error("unfinished string")),
                    Some(b'\n') => {
                        if !verbatim {
                            return Err(self.error("newline in a constant"));
                        }
                        buf.push(b'\n');
                        self.next();
                        self.current_line += 1;
                    }
                    Some(b'\\') => {
                        if verbatim {
                            buf.push(b'\\');
                            self.next();
                        } else {
                            self.next();
                            match self.cur {
                                Some(b'x') => {
                                    self.next();
                                    if !self.cur.is_some_and(|c| c.is_ascii_hexdigit()) {
                                        return Err(self.error("hexadecimal number expected"));
                                    }
                                    let mut value: u32 = 0;
                                    let mut n = 0;
                                    while let Some(c) = self.cur.filter(|c| c.is_ascii_hexdigit()) {
                                        if n >= 4 {
                                            break;
                                        }
                                        value = value * 16 + (c as char).to_digit(16).unwrap_or(0);
                                        n += 1;
                                        self.next();
                                    }
                                    buf.push(value as u8);
                                }
                                Some(e) => {
                                    let out = match e {
                                        b't' => b'\t',
                                        b'a' => 0x07,
                                        b'b' => 0x08,
                                        b'n' => b'\n',
                                        b'r' => b'\r',
                                        b'v' => 0x0b,
                                        b'f' => 0x0c,
                                        b'0' => 0,
                                        b'\\' => b'\\',
                                        b'"' => b'"',
                                        b'\'' => b'\'',
                                        _ => return Err(self.error("unrecognised escaper char")),
                                    };
                                    buf.push(out);
                                    self.next();
                                }
                                None => return Err(self.error("unfinished string")),
                            }
                        }
                    }
                    Some(c) => {
                        buf.push(c);
                        self.next();
                    }
                }
            }
            self.next();
            if verbatim && self.cur == Some(b'"') {
                buf.push(b'"');
                self.next();
            } else {
                break;
            }
        }
        if delim == b'\'' {
            if buf.is_empty() {
                return Err(self.error("empty constant"));
            }
            if buf.len() > 1 {
                return Err(self.error("constant too long"));
            }
            self.nvalue = buf[0] as i8 as i32;
            return Ok(Tok::Integer);
        }
        self.svalue = SqStr::new(&buf);
        Ok(Tok::StringLiteral)
    }

    /// `SQLexer::ReadNumber`.
    fn read_number(&mut self) -> LexResult<Tok> {
        #[derive(PartialEq)]
        enum Kind {
            Int,
            Float,
            Hex,
            Scientific,
            Octal,
        }
        let mut kind = Kind::Int;
        let first = self.cur.unwrap_or(b'0');
        let mut buf: Vec<u8> = Vec::new();
        self.next();
        let upper = self.cur.map(|c| c.to_ascii_uppercase());
        if first == b'0' && (upper == Some(b'X') || self.cur.is_some_and(is_octal)) {
            if self.cur.is_some_and(is_octal) {
                kind = Kind::Octal;
                while let Some(c) = self.cur.filter(|&c| is_octal(c)) {
                    buf.push(c);
                    self.next();
                }
                if self.cur.is_some_and(|c| c.is_ascii_digit()) {
                    return Err(self.error("invalid octal number"));
                }
            } else {
                self.next();
                kind = Kind::Hex;
                while let Some(c) = self.cur.filter(|c| c.is_ascii_hexdigit()) {
                    buf.push(c);
                    self.next();
                }
                // `MAX_HEX_DIGITS` is `sizeof(SQInteger)*2`, and `SQInteger`
                // is 32 bits.
                if buf.len() > 8 {
                    return Err(self.error("too many digits for an Hex number"));
                }
            }
        } else {
            buf.push(first);
            while let Some(c) = self
                .cur
                .filter(|&c| c == b'.' || c.is_ascii_digit() || c == b'e' || c == b'E')
            {
                if c == b'.' {
                    kind = Kind::Float;
                }
                if c == b'e' || c == b'E' {
                    if kind != Kind::Float {
                        return Err(self.error("invalid numeric format"));
                    }
                    kind = Kind::Scientific;
                    buf.push(c);
                    self.next();
                    if let Some(sign) = self.cur.filter(|&c| c == b'+' || c == b'-') {
                        buf.push(sign);
                        self.next();
                    }
                    if !self.cur.is_some_and(|c| c.is_ascii_digit()) {
                        return Err(self.error("exponent expected"));
                    }
                }
                if let Some(c) = self.cur {
                    buf.push(c);
                }
                self.next();
            }
        }
        match kind {
            Kind::Float | Kind::Scientific => {
                self.fvalue = strtod_prefix(&buf) as f32;
                Ok(Tok::Float)
            }
            Kind::Int => {
                let mut v: u32 = 0;
                for &c in &buf {
                    v = v.wrapping_mul(10).wrapping_add((c - b'0') as u32);
                }
                self.nvalue = v as i32;
                Ok(Tok::Integer)
            }
            Kind::Hex => {
                let mut v: u32 = 0;
                for &c in &buf {
                    v = v.wrapping_mul(16).wrapping_add((c as char).to_digit(16).unwrap_or(0));
                }
                self.nvalue = v as i32;
                Ok(Tok::Integer)
            }
            Kind::Octal => {
                let mut v: u32 = 0;
                for &c in &buf {
                    v = v.wrapping_mul(8).wrapping_add((c - b'0') as u32);
                }
                self.nvalue = v as i32;
                Ok(Tok::Integer)
            }
        }
    }

    fn read_id(&mut self) -> Tok {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            if let Some(c) = self.cur {
                buf.push(c);
            }
            self.next();
            if !self.cur.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
                break;
            }
        }
        let t = KEYWORDS
            .iter()
            .find(|(name, _)| name.as_bytes() == buf.as_slice())
            .map_or(Tok::Identifier, |(_, t)| *t);
        if t == Tok::Identifier || t == Tok::Constructor {
            self.svalue = SqStr::new(&buf);
        }
        t
    }
}

/// `strtod` over the longest prefix that parses, which is what `ReadNumber`
/// gets from a buffer like `1.2.3`: the lexer accepts any run of digits, dots
/// and exponents, and `strtod` quietly stops at the second dot.
pub fn strtod_prefix(bytes: &[u8]) -> f64 {
    let text = String::from_utf8_lossy(bytes);
    let mut best = 0.0;
    for end in (1..=text.len()).rev() {
        if let Ok(v) = text[..end].parse::<f64>() {
            best = v;
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(src: &str) -> Vec<Tok> {
        let mut lexer = Lexer::new(src.as_bytes());
        let mut out = Vec::new();
        loop {
            let t = lexer.lex().expect("lexes");
            if t == Tok::Eob {
                break;
            }
            out.push(t);
        }
        out
    }

    #[test]
    fn operators_are_the_longest_match() {
        assert_eq!(
            tokens("a <- b >>> 1 <= :: ... </ />"),
            vec![
                Tok::Identifier,
                Tok::NewSlot,
                Tok::Identifier,
                Tok::UShiftR,
                Tok::Integer,
                Tok::Le,
                Tok::DoubleColon,
                Tok::VarParams,
                Tok::AttrOpen,
                Tok::AttrClose,
            ]
        );
    }

    #[test]
    fn numbers_read_the_way_squirrel_2_reads_them() {
        let mut lexer = Lexer::new(b"0x1F 017 'a' 1.5 2147483648 1.0e2");
        assert_eq!(lexer.lex().unwrap(), Tok::Integer);
        assert_eq!(lexer.nvalue, 31);
        assert_eq!(lexer.lex().unwrap(), Tok::Integer);
        assert_eq!(lexer.nvalue, 15);
        assert_eq!(lexer.lex().unwrap(), Tok::Integer);
        assert_eq!(lexer.nvalue, 97);
        assert_eq!(lexer.lex().unwrap(), Tok::Float);
        assert_eq!(lexer.fvalue, 1.5);
        assert_eq!(lexer.lex().unwrap(), Tok::Integer);
        assert_eq!(lexer.nvalue, i32::MIN);
        assert_eq!(lexer.lex().unwrap(), Tok::Float);
        assert_eq!(lexer.fvalue, 100.0);
        // An exponent on an integer is refused rather than read.
        assert!(Lexer::new(b"1e5").lex().is_err());
    }

    #[test]
    fn the_previous_token_remembers_a_newline() {
        let mut lexer = Lexer::new(b"a\nb c");
        lexer.lex().unwrap();
        lexer.lex().unwrap();
        assert_eq!(lexer.prev_token, Prev::Newline);
        lexer.lex().unwrap();
        assert_eq!(lexer.prev_token, Prev::Token(Tok::Identifier));
    }

    #[test]
    fn strings_unescape_and_verbatim_strings_do_not() {
        let mut lexer = Lexer::new(br#""a\tb\x41" @"c\d""e""#);
        lexer.lex().unwrap();
        assert_eq!(lexer.svalue.as_bytes(), b"a\tbA");
        lexer.lex().unwrap();
        assert_eq!(lexer.svalue.as_bytes(), b"c\\d\"e");
    }
}
