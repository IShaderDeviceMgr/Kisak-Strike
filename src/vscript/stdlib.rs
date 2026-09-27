//! The parts of `sqstdlib` Valve registers (`vsquirrel.cpp:580`): the math
//! library, the string library with its `regexp` class, and the standard
//! error handler. The blob, I/O and system libraries are not registered in
//! Portal 2's VM and are not here.

use std::any::Any;

use super::format::{format_float, format_int, format_str, Spec};
use super::value::*;
use super::Vm;

type R = Result<Value, Value>;

fn err(message: &str) -> Value {
    Value::str(message)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

/// The C runtime's `rand()`, as the Windows CRT Portal 2 shipped against
/// implements it: `seed = seed * 214013 + 2531011`, bits 16..30. Seeded to 1,
/// as the CRT is before anything calls `srand`. Scripts use the engine's
/// `RandomInt` rather than this — no shipped script calls `rand` — so the
/// choice of CRT is recorded rather than load-bearing.
struct CRand(std::cell::Cell<u32>);

impl CRand {
    fn next(&self) -> i32 {
        let seed = self.0.get().wrapping_mul(214_013).wrapping_add(2_531_011);
        self.0.set(seed);
        ((seed >> 16) & 0x7fff) as i32
    }
}

pub(super) fn register_math(vm: &mut Vm) {
    let root = vm.root();
    let unary: [(&str, fn(f64) -> f64); 13] = [
        ("sqrt", f64::sqrt),
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("log", f64::ln),
        ("log10", f64::log10),
        ("tan", f64::tan),
        ("atan", f64::atan),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("exp", f64::exp),
        ("fabs", f64::abs),
    ];
    for (name, f) in unary {
        vm.register_native(&root, name, 2, ".n", move |_, _, a| {
            Ok(Value::Float(f(arg(a, 1).to_float() as f64) as f32))
        });
    }
    vm.register_native(&root, "atan2", 3, ".nn", |_, _, a| {
        let (y, x) = (arg(a, 1).to_float() as f64, arg(a, 2).to_float() as f64);
        Ok(Value::Float(y.atan2(x) as f32))
    });
    vm.register_native(&root, "pow", 3, ".nn", |_, _, a| {
        let (x, y) = (arg(a, 1).to_float() as f64, arg(a, 2).to_float() as f64);
        Ok(Value::Float(x.powf(y) as f32))
    });
    let rand = std::rc::Rc::new(CRand(std::cell::Cell::new(1)));
    let seed = rand.clone();
    vm.register_native(&root, "srand", 2, ".n", move |_, _, a| {
        seed.0.set(arg(a, 1).to_integer() as u32);
        Ok(Value::Null)
    });
    vm.register_native(&root, "rand", 1, "", move |_, _, _| Ok(Value::Integer(rand.next())));
    vm.register_native(&root, "abs", 2, ".n", |_, _, a| {
        Ok(Value::Integer(arg(a, 1).to_integer().wrapping_abs()))
    });
    vm.set_slot(&root, "RAND_MAX", Value::Integer(0x7fff));
    vm.set_slot(&root, "PI", Value::Float(std::f64::consts::PI as f32));
}

/// C's `isspace` in the "C" locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `sqstd_format`.
fn format(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    let Value::String(fmt) = arg(a, 1) else {
        return Ok(Value::Null);
    };
    let src = fmt.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let mut n = 0;
    let mut nparam = 2;
    while n < src.len() && src[n] != 0 {
        if src[n] != b'%' {
            out.push(src[n]);
            n += 1;
            continue;
        }
        if src.get(n + 1) == Some(&b'%') {
            out.push(b'%');
            n += 2;
            continue;
        }
        n += 1;
        if nparam >= a.len() {
            return Err(err("not enough paramters for the given format string"));
        }
        // `validate_format`.
        let start = n;
        let mut spec = Spec::default();
        while let Some(&c) = src.get(n) {
            match c {
                b'-' => spec.left = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'#' => spec.alt = true,
                b'0' => spec.zero = true,
                _ => break,
            }
            n += 1;
        }
        let mut digits = 0;
        let mut width = 0usize;
        while let Some(&c) = src.get(n).filter(|c| c.is_ascii_digit()) {
            width = width * 10 + (c - b'0') as usize;
            n += 1;
            digits += 1;
            if digits >= 3 {
                return Err(err("width format too long"));
            }
        }
        spec.width = width;
        if src.get(n) == Some(&b'.') {
            n += 1;
            let mut digits = 0;
            let mut precision = 0usize;
            while let Some(&c) = src.get(n).filter(|c| c.is_ascii_digit()) {
                precision = precision * 10 + (c - b'0') as usize;
                n += 1;
                digits += 1;
                if digits >= 3 {
                    return Err(err("precision format too long"));
                }
            }
            spec.precision = Some(precision);
        }
        if n - start > 20 {
            return Err(err("format too long"));
        }
        let conv = src.get(n).copied().unwrap_or(0);
        let value = arg(a, nparam);
        match conv {
            b's' => {
                let Value::String(s) = &value else {
                    return Err(err("string expected for the specified format"));
                };
                out.extend(format_str(s.as_bytes(), &spec));
            }
            b'i' | b'd' | b'c' | b'o' | b'u' | b'x' | b'X' => {
                if !value.is_numeric() {
                    return Err(err("integer expected for the specified format"));
                }
                let conv = if conv == b'i' { b'd' } else { conv };
                out.extend(format_int(value.to_integer(), conv, &spec));
            }
            b'f' | b'g' | b'G' | b'e' | b'E' => {
                if !value.is_numeric() {
                    return Err(err("float expected for the specified format"));
                }
                out.extend(format_float(value.to_float() as f64, conv, &spec).into_bytes());
            }
            _ => return Err(err("invalid format")),
        }
        n += 1;
        nparam += 1;
    }
    Ok(Value::bytes(&out))
}

pub(super) fn register_string(vm: &mut Vm) {
    let root = vm.root();
    vm.register_native(&root, "format", -2, ".s", format);
    vm.register_native(&root, "strip", 2, ".s", |_, _, a| {
        let Value::String(s) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let b = s.as_bytes();
        let start = b.iter().position(|&c| c == 0 || !is_space(c)).unwrap_or(b.len());
        let mut end = b.len();
        while end > start && is_space(b[end - 1]) {
            end -= 1;
        }
        Ok(Value::bytes(&b[start..end.max(start)]))
    });
    vm.register_native(&root, "lstrip", 2, ".s", |_, _, a| {
        let Value::String(s) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let b = s.as_bytes();
        let start = b.iter().position(|&c| c == 0 || !is_space(c)).unwrap_or(b.len());
        Ok(Value::bytes(&b[start..]))
    });
    vm.register_native(&root, "rstrip", 2, ".s", |_, _, a| {
        let Value::String(s) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let b = s.as_bytes();
        let mut end = b.len();
        // `__strip_r` never looks at the first character, so a string of
        // nothing but spaces keeps one.
        while end > 1 && is_space(b[end - 1]) {
            end -= 1;
        }
        Ok(Value::bytes(&b[..end]))
    });
    // `strtok` — any run of separators is one boundary, and empty tokens are
    // dropped.
    vm.register_native(&root, "split", 3, ".ss", |vm, _, a| {
        let (Value::String(s), Value::String(seps)) = (arg(a, 1), arg(a, 2)) else {
            return Ok(Value::Null);
        };
        if seps.len() == 0 {
            return Err(err("empty separators string"));
        }
        let text = s.as_bytes().split(|&c| c == 0).next().unwrap_or(&[]);
        let seps = seps.as_bytes();
        let parts: Vec<Value> = text
            .split(|c| seps.contains(c))
            .filter(|t| !t.is_empty())
            .map(Value::bytes)
            .collect();
        Ok(Value::Array(vm.new_array(parts)))
    });
    register_regexp(vm);
}

/// `sqstd_seterrorhandlers` — the runtime error handler that prints the
/// error, the call stack and the locals of the innermost ten frames.
pub(super) fn set_error_handlers(vm: &mut Vm) {
    vm.error_handler = vm.native("printerror", 0, "", |vm, host, a| {
        match a.get(1) {
            Some(Value::String(s)) => {
                vm.print(&format!("\nAN ERROR HAS OCCURED [{}]\n", s.to_string_lossy()))
            }
            _ => vm.print("\nAN ERROR HAS OCCURED [unknown]\n"),
        }
        print_callstack(vm, host);
        Ok(Value::Null)
    });
}

/// `sqstd_printcallstack`.
fn print_callstack(vm: &mut Vm, _: &mut dyn Any) {
    let mut text = String::from("\nCALLSTACK\n");
    let mut level = 1;
    while let Some((func, src, line)) = vm.stack_infos(level) {
        text.push_str(&format!("*FUNCTION [{func}()] {src} line [{line}]\n"));
        level += 1;
    }
    text.push_str("\nLOCALS\n");
    for level in 0..10 {
        for (name, value) in vm.stack_locals(level) {
            let shown = match &value {
                Value::Null => "NULL".to_owned(),
                Value::Integer(i) => i.to_string(),
                Value::Float(f) => super::format::format_g(*f as f64, 14, false),
                Value::String(s) => format!("\"{}\"", s.to_string_lossy()),
                Value::Table(_) => "TABLE".to_owned(),
                Value::Array(_) => "ARRAY".to_owned(),
                Value::Closure(_) => "CLOSURE".to_owned(),
                Value::Native(_) => "NATIVECLOSURE".to_owned(),
                Value::Class(_) => "CLASS".to_owned(),
                Value::Instance(_) => "INSTANCE".to_owned(),
                Value::WeakRef(_) => "WEAKREF".to_owned(),
                Value::Bool(b) => b.to_string(),
            };
            text.push_str(&format!("[{name}] {shown}\n"));
        }
    }
    vm.print(&text);
}

// ----- regexp: `sqstdrex.cpp` ----------------------------------------------

/// Node types above any character (`MAX_CHAR` is 0xFF).
const OP_GREEDY: i32 = 0x100;
const OP_OR: i32 = 0x101;
const OP_EXPR: i32 = 0x102;
const OP_NOCAPEXPR: i32 = 0x103;
const OP_DOT: i32 = 0x104;
const OP_CLASS: i32 = 0x105;
const OP_CCLASS: i32 = 0x106;
const OP_NCLASS: i32 = 0x107;
const OP_RANGE: i32 = 0x108;
const OP_EOL: i32 = 0x10A;
const OP_BOL: i32 = 0x10B;
const OP_WB: i32 = 0x10C;

#[derive(Clone, Copy)]
struct RexNode {
    kind: i32,
    left: i32,
    right: i32,
    next: i32,
}

/// `SQRex` — Squirrel's own small backtracking regex, kept rather than
/// replaced because its dialect (`\p` is punctuation, `{n,m}` caps at 65535,
/// `$` only at the very end) is what a script's pattern was written against.
pub(super) struct Rex {
    nodes: Vec<RexNode>,
    first: i32,
    nsubexpr: i32,
    matches: Vec<(usize, usize)>,
    currsubexp: i32,
    bol: usize,
    eol: usize,
}

/// A signed-`char` read, with the NUL past the end that the C sees.
fn ch(text: &[u8], i: usize) -> i32 {
    text.get(i).map_or(0, |&c| c as i8 as i32)
}

struct RexCompiler<'a> {
    p: &'a [u8],
    pos: usize,
    rex: Rex,
}

impl RexCompiler<'_> {
    fn cur(&self) -> i32 {
        ch(self.p, self.pos)
    }

    fn new_node(&mut self, kind: i32) -> i32 {
        let mut n = RexNode {
            kind,
            left: -1,
            right: -1,
            next: -1,
        };
        if kind == OP_EXPR {
            n.right = self.rex.nsubexpr;
            self.rex.nsubexpr += 1;
        }
        self.rex.nodes.push(n);
        self.rex.nodes.len() as i32 - 1
    }

    fn node(&mut self, i: i32) -> &mut RexNode {
        &mut self.rex.nodes[i as usize]
    }

    fn expect(&mut self, c: u8) -> Result<(), &'static str> {
        if self.cur() != c as i32 {
            return Err("expected paren");
        }
        self.pos += 1;
        Ok(())
    }

    fn is_print(c: i32) -> bool {
        (0x20..0x7f).contains(&c)
    }

    fn escape_char(&mut self) -> Result<i32, &'static str> {
        if self.cur() == b'\\' as i32 {
            self.pos += 1;
            let c = self.cur();
            self.pos += 1;
            return Ok(match c as u8 {
                b'v' => 0x0b,
                b'n' => b'\n' as i32,
                b't' => b'\t' as i32,
                b'r' => b'\r' as i32,
                b'f' => 0x0c,
                _ => c,
            });
        } else if !Self::is_print(self.cur()) {
            return Err("letter expected");
        }
        let c = self.cur();
        self.pos += 1;
        Ok(c)
    }

    fn char_class(&mut self, class: i32) -> i32 {
        let n = self.new_node(OP_CCLASS);
        self.node(n).left = class;
        n
    }

    fn char_node(&mut self, is_class: bool) -> Result<i32, &'static str> {
        if self.cur() == b'\\' as i32 {
            self.pos += 1;
            let c = self.cur();
            match c as u8 {
                b'n' => {
                    self.pos += 1;
                    return Ok(self.new_node(b'\n' as i32));
                }
                b't' => {
                    self.pos += 1;
                    return Ok(self.new_node(b'\t' as i32));
                }
                b'r' => {
                    self.pos += 1;
                    return Ok(self.new_node(b'\r' as i32));
                }
                b'f' => {
                    self.pos += 1;
                    return Ok(self.new_node(0x0c));
                }
                b'v' => {
                    self.pos += 1;
                    return Ok(self.new_node(0x0b));
                }
                b'a' | b'A' | b'w' | b'W' | b's' | b'S' | b'd' | b'D' | b'x' | b'X' | b'c'
                | b'C' | b'p' | b'P' | b'l' | b'u' => {
                    self.pos += 1;
                    return Ok(self.char_class(c));
                }
                b'b' | b'B' if !is_class => {
                    let node = self.new_node(OP_WB);
                    self.node(node).left = c;
                    self.pos += 1;
                    return Ok(node);
                }
                _ => {
                    self.pos += 1;
                    return Ok(self.new_node(c));
                }
            }
        } else if !Self::is_print(self.cur()) {
            return Err("letter expected");
        }
        let c = self.cur();
        self.pos += 1;
        Ok(self.new_node(c))
    }

    fn class(&mut self) -> Result<i32, &'static str> {
        let ret = if self.cur() == b'^' as i32 {
            let r = self.new_node(OP_NCLASS);
            self.pos += 1;
            r
        } else {
            self.new_node(OP_CLASS)
        };
        if self.cur() == b']' as i32 {
            return Err("empty class");
        }
        let mut chain = ret;
        let mut first = -1;
        // `exp->_eol` is still null while compiling, so the C's second test
        // never stops the loop: an unterminated class runs into the NUL and
        // fails as "letter expected".
        while self.cur() != b']' as i32 {
            if self.cur() == b'-' as i32 && first != -1 {
                // `if(*exp->_p++ == ']')` tests the `-` itself, so it never
                // fires; the increment is what matters.
                self.pos += 1;
                let r = self.new_node(OP_RANGE);
                // `if(first>*exp->_p)` compares a *node index* with a
                // character — Squirrel's bug, kept.
                if first > self.cur() {
                    return Err("invalid range");
                }
                if self.rex.nodes[first as usize].kind == OP_CCLASS {
                    return Err("cannot use character classes in ranges");
                }
                let left = self.rex.nodes[first as usize].kind;
                self.node(r).left = left;
                let t = self.escape_char()?;
                self.node(r).right = t;
                self.node(chain).next = r;
                chain = r;
                first = -1;
            } else if first != -1 {
                let c = first;
                self.node(chain).next = c;
                chain = c;
                first = self.char_node(true)?;
            } else {
                first = self.char_node(true)?;
            }
        }
        if first != -1 {
            self.node(chain).next = first;
        }
        let head = self.node(ret).next;
        self.node(ret).left = head;
        self.node(ret).next = -1;
        Ok(ret)
    }

    fn parse_number(&mut self) -> Result<i32, &'static str> {
        let mut ret = self.cur() - b'0' as i32;
        let mut positions: i64 = 10;
        self.pos += 1;
        while (self.cur() as u8).is_ascii_digit() && self.cur() >= 0 {
            ret = ret * 10 + (self.cur() - b'0' as i32);
            self.pos += 1;
            if positions == 1_000_000_000 {
                return Err("overflow in numeric constant");
            }
            positions *= 10;
        }
        Ok(ret)
    }

    fn element(&mut self) -> Result<i32, &'static str> {
        let mut ret = match self.cur() as u8 {
            b'(' => {
                self.pos += 1;
                let expr = if self.cur() == b'?' as i32 {
                    self.pos += 1;
                    self.expect(b':')?;
                    self.new_node(OP_NOCAPEXPR)
                } else {
                    self.new_node(OP_EXPR)
                };
                let inner = self.list()?;
                self.node(expr).left = inner;
                self.expect(b')')?;
                expr
            }
            b'[' => {
                self.pos += 1;
                let r = self.class()?;
                self.expect(b']')?;
                r
            }
            b'$' => {
                self.pos += 1;
                self.new_node(OP_EOL)
            }
            b'.' => {
                self.pos += 1;
                self.new_node(OP_DOT)
            }
            _ => self.char_node(false)?,
        };
        let (mut p0, mut p1): (u32, u32) = (0, 0);
        let mut greedy = false;
        match self.cur() as u8 {
            b'*' => {
                p1 = 0xFFFF;
                self.pos += 1;
                greedy = true;
            }
            b'+' => {
                p0 = 1;
                p1 = 0xFFFF;
                self.pos += 1;
                greedy = true;
            }
            b'?' => {
                p1 = 1;
                self.pos += 1;
                greedy = true;
            }
            b'{' => {
                self.pos += 1;
                if !(self.cur() as u8).is_ascii_digit() {
                    return Err("number expected");
                }
                p0 = self.parse_number()? as u16 as u32;
                match self.cur() as u8 {
                    b'}' => {
                        p1 = p0;
                        self.pos += 1;
                    }
                    b',' => {
                        self.pos += 1;
                        p1 = 0xFFFF;
                        if (self.cur() as u8).is_ascii_digit() {
                            p1 = self.parse_number()? as u16 as u32;
                        }
                        self.expect(b'}')?;
                    }
                    _ => return Err(", or } expected"),
                }
                greedy = true;
            }
            _ => {}
        }
        if greedy {
            let n = self.new_node(OP_GREEDY);
            self.node(n).left = ret;
            self.node(n).right = ((p0 << 16) | p1) as i32;
            ret = n;
        }
        let c = self.cur();
        if c != b'|' as i32 && c != b')' as i32 && c != b'*' as i32 && c != b'+' as i32 && c != 0 {
            let n = self.element()?;
            self.node(ret).next = n;
        }
        Ok(ret)
    }

    fn list(&mut self) -> Result<i32, &'static str> {
        let mut ret = -1;
        if self.cur() == b'^' as i32 {
            self.pos += 1;
            ret = self.new_node(OP_BOL);
        }
        let e = self.element()?;
        if ret != -1 {
            self.node(ret).next = e;
        } else {
            ret = e;
        }
        if self.cur() == b'|' as i32 {
            self.pos += 1;
            let temp = self.new_node(OP_OR);
            self.node(temp).left = ret;
            let right = self.list()?;
            self.node(temp).right = right;
            ret = temp;
        }
        Ok(ret)
    }
}

impl Rex {
    /// `sqstd_rex_compile`.
    pub(super) fn compile(pattern: &[u8]) -> Result<Rex, &'static str> {
        let pattern = pattern.split(|&c| c == 0).next().unwrap_or(&[]);
        let mut c = RexCompiler {
            p: pattern,
            pos: 0,
            rex: Rex {
                nodes: Vec::new(),
                first: 0,
                nsubexpr: 0,
                matches: Vec::new(),
                currsubexp: 0,
                bol: 0,
                eol: 0,
            },
        };
        c.rex.first = c.new_node(OP_EXPR);
        let res = c.list()?;
        let first = c.rex.first;
        c.node(first).left = res;
        if c.cur() != 0 {
            return Err("unexpected character");
        }
        c.rex.matches = vec![(0, 0); c.rex.nsubexpr as usize];
        Ok(c.rex)
    }

    fn match_cclass(class: i32, c: i32) -> bool {
        let b = c as u8;
        let ok = c >= 0;
        match class as u8 {
            b'a' => ok && b.is_ascii_alphabetic(),
            b'A' => !(ok && b.is_ascii_alphabetic()),
            b'w' => (ok && b.is_ascii_alphanumeric()) || b == b'_',
            b'W' => !(ok && b.is_ascii_alphanumeric()) && b != b'_',
            b's' => ok && is_space(b),
            b'S' => !(ok && is_space(b)),
            b'd' => ok && b.is_ascii_digit(),
            b'D' => !(ok && b.is_ascii_digit()),
            b'x' => ok && b.is_ascii_hexdigit(),
            b'X' => !(ok && b.is_ascii_hexdigit()),
            b'c' => ok && b.is_ascii_control(),
            b'C' => !(ok && b.is_ascii_control()),
            b'p' => ok && b.is_ascii_punctuation(),
            b'P' => !(ok && b.is_ascii_punctuation()),
            b'l' => ok && b.is_ascii_lowercase(),
            b'u' => ok && b.is_ascii_uppercase(),
            _ => false,
        }
    }

    fn match_class(&self, mut node: i32, c: i32) -> bool {
        loop {
            let n = self.nodes[node as usize];
            match n.kind {
                OP_RANGE => {
                    if c >= n.left && c <= n.right {
                        return true;
                    }
                }
                OP_CCLASS => {
                    if Self::match_cclass(n.left, c) {
                        return true;
                    }
                }
                kind => {
                    if c == kind {
                        return true;
                    }
                }
            }
            if n.next == -1 {
                return false;
            }
            node = n.next;
        }
    }

    /// `sqstd_rex_matchnode`, with positions for pointers.
    fn match_node(&mut self, text: &[u8], node: i32, s: usize, next: i32) -> Option<usize> {
        let n = self.nodes[node as usize];
        match n.kind {
            OP_GREEDY => {
                let p0 = (n.right >> 16) & 0xFFFF;
                let p1 = n.right & 0xFFFF;
                let mut nmatches = 0;
                let mut s = s;
                let mut good = s;
                let greedystop = if n.next != -1 { n.next } else { next };
                while nmatches == 0xFFFF || nmatches < p1 {
                    let Some(ns) = self.match_node(text, n.left, s, greedystop) else {
                        break;
                    };
                    s = ns;
                    nmatches += 1;
                    good = s;
                    if greedystop != -1 {
                        let g = self.nodes[greedystop as usize];
                        if g.kind != OP_GREEDY || ((g.right >> 16) & 0xFFFF) != 0 {
                            let gnext = if g.next != -1 {
                                g.next
                            } else if next != -1 && self.nodes[next as usize].next != -1 {
                                self.nodes[next as usize].next
                            } else {
                                -1
                            };
                            if self.match_node(text, greedystop, s, gnext).is_some()
                                && ((p0 == p1 && p0 == nmatches)
                                    || (nmatches >= p0 && p1 == 0xFFFF)
                                    || (nmatches >= p0 && nmatches <= p1))
                            {
                                break;
                            }
                        }
                    }
                    if s >= self.eol {
                        break;
                    }
                }
                if (p0 == p1 && p0 == nmatches)
                    || (nmatches >= p0 && p1 == 0xFFFF)
                    || (nmatches >= p0 && nmatches <= p1)
                {
                    return Some(good);
                }
                None
            }
            OP_OR => {
                for side in [n.left, n.right] {
                    let mut asd = Some(s);
                    let mut temp = side;
                    while let Some(a) = asd {
                        asd = self.match_node(text, temp, a, -1);
                        let Some(found) = asd else { break };
                        let t = self.nodes[temp as usize];
                        if t.next != -1 {
                            temp = t.next;
                        } else {
                            return Some(found);
                        }
                    }
                }
                None
            }
            OP_EXPR | OP_NOCAPEXPR => {
                let mut cur = s;
                let mut sub = n.left;
                let mut capture = -1;
                if n.kind != OP_NOCAPEXPR && n.right == self.currsubexp {
                    capture = self.currsubexp;
                    self.matches[capture as usize] = (cur, 0);
                    self.currsubexp += 1;
                }
                loop {
                    let sn = self.nodes[sub as usize];
                    let subnext = if sn.next != -1 { sn.next } else { next };
                    match self.match_node(text, sub, cur, subnext) {
                        Some(c) => cur = c,
                        None => {
                            if capture != -1 {
                                self.matches[capture as usize] = (0, 0);
                            }
                            return None;
                        }
                    }
                    if sn.next == -1 {
                        break;
                    }
                    sub = sn.next;
                }
                if capture != -1 {
                    let begin = self.matches[capture as usize].0;
                    self.matches[capture as usize].1 = cur - begin;
                }
                Some(cur)
            }
            OP_WB => {
                let sp = |i: Option<usize>| i.is_some_and(|i| is_space(text.get(i).copied().unwrap_or(0)));
                let here = sp(Some(s));
                let at_boundary = (s == self.bol && !here)
                    || (s == self.eol && !sp(s.checked_sub(1)))
                    || (!here && sp(Some(s + 1)))
                    || (here && !sp(Some(s + 1)));
                let is_b = n.left == b'b' as i32;
                match at_boundary == is_b {
                    true => Some(s),
                    false => None,
                }
            }
            OP_BOL => (s == self.bol).then_some(s),
            OP_EOL => (s == self.eol).then_some(s),
            OP_DOT => Some(s + 1),
            OP_NCLASS | OP_CLASS => {
                let hit = self.match_class(n.left, ch(text, s));
                if hit == (n.kind == OP_CLASS) {
                    Some(s + 1)
                } else {
                    None
                }
            }
            OP_CCLASS => Self::match_cclass(n.left, ch(text, s)).then_some(s + 1),
            kind => (ch(text, s) == kind).then_some(s + 1),
        }
    }

    /// `sqstd_rex_match` — the whole string.
    fn is_match(&mut self, text: &[u8]) -> bool {
        self.bol = 0;
        self.eol = text.len();
        self.currsubexp = 0;
        self.match_node(text, 0, 0, -1) == Some(self.eol)
    }

    /// `sqstd_rex_searchrange` from `start`.
    fn search(&mut self, text: &[u8], start: usize) -> Option<(usize, usize)> {
        let end = text.len();
        let mut begin = start;
        if begin >= end {
            return None;
        }
        self.bol = start;
        self.eol = end;
        let mut cur;
        loop {
            cur = Some(begin);
            let mut node = self.first;
            while node != -1 {
                self.currsubexp = 0;
                cur = self.match_node(text, node, cur.expect("still matching"), -1);
                if cur.is_none() {
                    break;
                }
                node = self.nodes[node as usize].next;
            }
            begin += 1;
            if cur.is_some() || begin == end {
                break;
            }
        }
        let cur = cur?;
        Some((begin - 1, cur))
    }
}

fn with_rex<T>(this: &Value, f: impl FnOnce(&mut Rex) -> T) -> Option<T> {
    let Value::Instance(i) = this else {
        return None;
    };
    let mut i = i.borrow_mut();
    let rex = i.user.as_mut()?.downcast_mut::<Rex>()?;
    Some(f(rex))
}

fn rex_match_table(vm: &Vm, begin: usize, end: usize) -> Value {
    let t = vm.new_table(0);
    {
        let mut t = t.borrow_mut();
        t.table.new_slot(Value::str("begin"), Value::Integer(begin as i32));
        t.table.new_slot(Value::str("end"), Value::Integer(end as i32));
    }
    Value::Table(t)
}

fn register_regexp(vm: &mut Vm) {
    let class = vm.new_class(None, 0);
    let slot = |vm: &mut Vm, name: &str, n: i32, mask: &str, f: fn(&mut Vm, &mut dyn Any, &[Value]) -> R| {
        let native = vm.native(name, n, mask, f);
        vm.class_new_slot(&class, name, native);
    };
    slot(vm, "constructor", 2, ".s", |_, _, a| {
        let Value::String(pattern) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let rex = Rex::compile(pattern.as_bytes()).map_err(err)?;
        if let Value::Instance(i) = arg(a, 0) {
            i.borrow_mut().user = Some(Box::new(rex));
        }
        Ok(Value::Null)
    });
    slot(vm, "search", -2, "xsn", |vm, _, a| {
        let Value::String(s) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let start = if a.len() > 2 { arg(a, 2).to_integer().max(0) as usize } else { 0 };
        let text = s.as_bytes().split(|&c| c == 0).next().unwrap_or(&[]).to_vec();
        let found = with_rex(&arg(a, 0), |rex| rex.search(&text, start)).flatten();
        Ok(found.map_or(Value::Null, |(b, e)| rex_match_table(vm, b, e)))
    });
    slot(vm, "match", 2, "xs", |_, _, a| {
        let Value::String(s) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let text = s.as_bytes().split(|&c| c == 0).next().unwrap_or(&[]).to_vec();
        Ok(Value::Bool(with_rex(&arg(a, 0), |rex| rex.is_match(&text)).unwrap_or(false)))
    });
    slot(vm, "capture", -2, "xsn", |vm, _, a| {
        let Value::String(s) = arg(a, 1) else {
            return Ok(Value::Null);
        };
        let start = if a.len() > 2 { arg(a, 2).to_integer().max(0) as usize } else { 0 };
        let text = s.as_bytes().split(|&c| c == 0).next().unwrap_or(&[]).to_vec();
        let captures = with_rex(&arg(a, 0), |rex| {
            rex.search(&text, start).map(|_| rex.matches.clone())
        })
        .flatten();
        let Some(captures) = captures else {
            return Ok(Value::Null);
        };
        let items = captures
            .into_iter()
            .map(|(b, len)| match len > 0 {
                true => rex_match_table(vm, b, b + len),
                false => rex_match_table(vm, 0, 0),
            })
            .collect();
        Ok(Value::Array(vm.new_array(items)))
    });
    slot(vm, "subexpcount", 1, "x", |_, _, a| {
        Ok(Value::Integer(with_rex(&arg(a, 0), |rex| rex.nsubexpr).unwrap_or(0)))
    });
    slot(vm, "_typeof", 1, "x", |_, _, _| Ok(Value::str("regexp")));
    let root = vm.root();
    vm.set_slot(&root, "regexp", Value::Class(class));
}
