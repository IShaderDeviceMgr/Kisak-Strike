//! What [`parser`](super::parser) produces and [`interp`](super::interp) runs.
//!
//! Squirrel compiles to a register bytecode (`sqopcodes.h`); this port
//! evaluates a tree instead. **What carries across is every decision the
//! compiler makes**: names are resolved at compile time exactly as
//! `SQCompiler::Factor` resolves them (a local, then an explicit outer, then a
//! constant, then a field of `this`), constants are substituted, and the
//! shapes that decide what `this` a call gets — `obj.f()` against `f()` against
//! `(f)()` — are kept distinct, because `_OP_PREPCALL` and `_OP_MOVE target, 0`
//! give different answers.

use std::rc::Rc;

use super::value::{SqStr, Value};

/// `SQFunctionProto`.
pub struct FuncProto {
    /// The function's name, or null for a function expression.
    pub name: Value,
    pub source: SqStr,
    /// `_parameters`, beginning with `this`.
    pub params: Vec<SqStr>,
    /// How many of the trailing parameters have defaults.
    pub n_defaults: usize,
    /// The expressions for those defaults, **evaluated in the enclosing
    /// function** when the closure is made — `CreateFunction` compiles them
    /// into the parent's function state.
    pub default_exprs: Vec<Expr>,
    pub varparams: bool,
    pub outers: Vec<Outer>,
    /// Slots for `this`, the parameters and every local, block-scoped slots
    /// reused as the compiler reuses stack positions.
    pub stack_size: usize,
    pub body: Stmt,
    /// `_bgenerator` — the body contains a `yield`.
    pub generator: bool,
    /// The line the function starts on, for a stack trace.
    pub line: u32,
}

/// `SQOuterVar` — what a closure copies when it is made.
#[derive(Clone)]
pub enum Outer {
    /// `otLOCAL`: a local slot of the enclosing function.
    Local(u16),
    /// `otOUTER`: one of the enclosing function's own outers.
    Outer(u16),
    /// `otSYMBOL`: looked up by name on the enclosing `this` (and the root
    /// table), and an error if it is not there.
    Symbol(SqStr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl ArithOp {
    pub fn symbol(self) -> char {
        match self {
            ArithOp::Add => '+',
            ArithOp::Sub => '-',
            ArithOp::Mul => '*',
            ArithOp::Div => '/',
            ArithOp::Mod => '%',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitOp {
    And,
    Or,
    Xor,
    ShiftL,
    ShiftR,
    UShiftR,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Arith(ArithOp),
    Bit(BitOp),
    Cmp(CmpOp),
    Eq,
    Ne,
    /// `key in container`.
    In,
    /// `instance instanceof class`.
    InstanceOf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
    Typeof,
    Clone,
    Resume,
}

pub enum Expr {
    Null,
    Bool(bool),
    Int(i32),
    Float(f32),
    Str(SqStr),
    /// A compile-time constant — `const` or an `enum` member. Reads like a
    /// value and cannot be assigned: the compiler marks it `_freevar`.
    Const(Value),
    Local(u16),
    Outer(u16),
    /// `::` — `_OP_LOADROOTTABLE`.
    Root,
    /// `obj.key` or `obj[key]` — `_OP_GET`, with the root-table fallback when
    /// `obj` is the function's own `this`.
    Get(Box<Expr>, Box<Expr>),
    /// `obj.parent`, or a bare `parent` (on `this`).
    Parent(Box<Expr>),
    Vargc,
    Vargv(Box<Expr>),
    /// `(expr)`. Kept as a node because it changes two things: it cannot be
    /// assigned to, and calling it does not bind `this` to the object the
    /// function came from.
    Group(Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Array(Vec<Expr>),
    /// A table literal and its key count, which sizes the table.
    Table(Vec<(Expr, Expr)>),
    Function(Rc<FuncProto>),
    Class(Box<ClassExpr>),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `target = value`. The target is a `Local` or a `Get`.
    Assign(Box<Expr>, Box<Expr>),
    /// `target <- value`. The target is a `Get`.
    NewSlot(Box<Expr>, Box<Expr>),
    /// `target op= value`. A target that is not a `Local` or a `Get` — an
    /// outer or a constant — is computed and not stored, which is what
    /// `_OP_COMPARITHL` on a temporary register does.
    Compound(ArithOp, Box<Expr>, Box<Expr>),
    /// `++x` / `--x`; the delta is ±1. A non-storable operand is computed and
    /// not stored, as above.
    PreIncr(Box<Expr>, i32),
    /// `x++` / `x--`.
    PostIncr(Box<Expr>, i32),
    /// `delete obj.key`.
    Delete(Box<Expr>),
    /// `delegate d : t` — makes `d` the delegate of `t`, and is `t`.
    Delegate(Box<Expr>, Box<Expr>),
    /// `a, b, c` — every one evaluated, the last one the value.
    Comma(Vec<Expr>),
}

/// A class body.
pub struct ClassExpr {
    pub base: Option<Expr>,
    pub attributes: Option<Expr>,
    pub members: Vec<ClassMember>,
}

pub struct ClassMember {
    pub attributes: Option<Expr>,
    pub is_static: bool,
    pub key: Expr,
    pub value: Expr,
}

pub struct Stmt {
    pub line: u32,
    pub kind: StmtKind,
}

pub enum StmtKind {
    Empty,
    Expr(Expr),
    /// `local a = x, b`. Each is evaluated and stored in turn, so `b`'s
    /// initialiser can read `a`.
    Local(Vec<(SqStr, u16, Option<Expr>)>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    Foreach {
        key: (SqStr, u16),
        value: (SqStr, u16),
        container: Expr,
        body: Box<Stmt>,
    },
    Switch {
        value: Expr,
        cases: Vec<(Expr, Vec<Stmt>)>,
        default: Option<Vec<Stmt>>,
    },
    Block(Vec<Stmt>),
    Return(Option<Expr>),
    /// `yield` — parsed, and an error when it runs: a function containing one
    /// is a generator, and calling a generator is refused (no shipped script
    /// has one), so the only `yield` that can execute is one outside any
    /// function, which Squirrel refuses too. Its value is never evaluated.
    Yield,
    Break,
    Continue,
    Try {
        body: Box<Stmt>,
        catch: (SqStr, u16),
        handler: Box<Stmt>,
    },
    Throw(Expr),
}
