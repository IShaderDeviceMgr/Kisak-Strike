//! `SQCompiler` (`sqcompiler.cpp`) — Squirrel 2.2 source to [`ast`](super::ast).
//!
//! The grammar is Valve's copy of Squirrel's, rule for rule, **including the
//! parts that are not what a C programmer expects**:
//!
//! - `==` and `<` are one precedence level, and `in`/`instanceof` sit between
//!   `&&` and `|`.
//! - A newline ends a statement, and one before `[` is an error rather than an
//!   index ("cannot brake deref"), but one before `(` is still a call.
//! - Function arguments, array elements and table slots need no separator.
//! - A function sees an enclosing function's locals only if it names them in
//!   `function(...):(a, b)`, and gets copies; anything else unqualified is a
//!   field of `this`.
//! - `const` and `enum` are compile-time: they go into the VM's constant table
//!   while the file is being *compiled*, and every later compile sees them.

use std::rc::Rc;

use super::ast::*;
use super::lexer::{CompileError, Lexer, Prev, Tok};
use super::value::{SqStr, TableObj, TableRef, Value};

struct FuncState {
    locals: Vec<(SqStr, u16)>,
    outers: Vec<(SqStr, Outer)>,
    top: u16,
    max: u16,
    /// Breakable and continuable nesting — `_breaktargets` and
    /// `_continuetargets`, reduced to counts because the jumps they hold are
    /// the evaluator's business here.
    breaks: u32,
    continues: u32,
    generator: bool,
}

impl FuncState {
    fn new() -> FuncState {
        FuncState {
            locals: Vec::new(),
            outers: Vec::new(),
            top: 0,
            max: 0,
            breaks: 0,
            continues: 0,
            generator: false,
        }
    }

    fn push_local(&mut self, name: SqStr) -> u16 {
        let slot = self.top;
        self.locals.push((name, slot));
        self.top += 1;
        self.max = self.max.max(self.top);
        slot
    }

    /// `SQFuncState::SetStackSize` — the locals above `size` go out of scope.
    fn set_top(&mut self, size: u16) {
        self.locals.retain(|(_, slot)| *slot < size);
        self.top = size;
    }

    fn local(&self, name: &SqStr) -> Option<u16> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, slot)| *slot)
    }

    fn outer(&self, name: &SqStr) -> Option<u16> {
        self.outers
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| i as u16)
    }
}

pub struct Parser<'a> {
    lex: Lexer<'a>,
    token: Tok,
    consts: TableRef,
    source: SqStr,
    fs: Vec<FuncState>,
}

type PResult<T> = Result<T, CompileError>;

/// Compiles a whole source file into its `main` function.
pub fn compile(source: &[u8], name: &str, consts: &TableRef) -> PResult<Rc<FuncProto>> {
    let mut parser = Parser {
        lex: Lexer::new(source),
        token: Tok::Eob,
        consts: consts.clone(),
        source: SqStr::from_str(name),
        fs: vec![FuncState::new()],
    };
    parser.compile_main()
}

impl<'a> Parser<'a> {
    fn f(&mut self) -> &mut FuncState {
        self.fs.last_mut().expect("a function is being compiled")
    }

    fn error<T>(&self, message: impl Into<String>) -> PResult<T> {
        Err(CompileError {
            message: message.into(),
            line: self.lex.current_line,
            column: self.lex.current_column,
        })
    }

    fn advance(&mut self) -> PResult<()> {
        self.token = self.lex.lex()?;
        Ok(())
    }

    fn line(&self) -> u32 {
        self.lex.current_line
    }

    /// `SQCompiler::Expect`, for the tokens that carry no value.
    fn expect(&mut self, tok: Tok) -> PResult<()> {
        if self.token != tok {
            return self.expected(tok);
        }
        self.advance()
    }

    fn expected<T>(&self, tok: Tok) -> PResult<T> {
        match tok {
            Tok::Char(c) => self.error(format!("expected '{}'", c as char)),
            other => self.error(format!("expected '{}'", other.describe())),
        }
    }

    /// `Expect( TK_IDENTIFIER )`, which also takes `constructor`.
    fn expect_ident(&mut self) -> PResult<SqStr> {
        if self.token != Tok::Identifier && self.token != Tok::Constructor {
            return self.expected(Tok::Identifier);
        }
        let name = self.lex.svalue.clone();
        self.advance()?;
        Ok(name)
    }

    fn is_end_of_statement(&self) -> bool {
        self.lex.prev_token == Prev::Newline
            || self.token == Tok::Eob
            || self.token == Tok::Char(b'}')
            || self.token == Tok::Char(b';')
    }

    fn optional_semicolon(&mut self) -> PResult<()> {
        if self.token == Tok::Char(b';') {
            return self.advance();
        }
        if !self.is_end_of_statement() {
            return self.error("end of statement expected (; or lf)");
        }
        Ok(())
    }

    fn prev_is(&self, c: u8) -> bool {
        self.lex.prev_token == Prev::Token(Tok::Char(c))
    }

    fn compile_main(&mut self) -> PResult<Rc<FuncProto>> {
        self.f().push_local(SqStr::from_str("this"));
        self.advance()?;
        let mut body = Vec::new();
        while self.token != Tok::Eob {
            body.push(self.statement()?);
            if !self.prev_is(b'}') {
                self.optional_semicolon()?;
            }
        }
        let fs = self.fs.pop().expect("main's state");
        Ok(Rc::new(FuncProto {
            name: Value::str("main"),
            source: self.source.clone(),
            params: vec![SqStr::from_str("this")],
            n_defaults: 0,
            default_exprs: Vec::new(),
            varparams: false,
            outers: Vec::new(),
            stack_size: fs.max as usize,
            body: Stmt {
                line: 1,
                kind: StmtKind::Block(body),
            },
            generator: fs.generator,
            line: 1,
        }))
    }

    /// `SQCompiler::Statements` — up to a `}`, `case` or `default`.
    fn statements(&mut self) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        while self.token != Tok::Char(b'}')
            && self.token != Tok::Default
            && self.token != Tok::Case
            && self.token != Tok::Eob
        {
            out.push(self.statement()?);
            if !self.prev_is(b'}') && !self.prev_is(b';') {
                self.optional_semicolon()?;
            }
        }
        Ok(out)
    }

    fn statement(&mut self) -> PResult<Stmt> {
        let line = self.line();
        let kind = match self.token {
            Tok::Char(b';') => {
                self.advance()?;
                StmtKind::Empty
            }
            Tok::If => self.if_statement()?,
            Tok::While => self.while_statement()?,
            Tok::Do => self.do_while_statement()?,
            Tok::For => self.for_statement()?,
            Tok::Foreach => self.foreach_statement()?,
            Tok::Switch => self.switch_statement()?,
            Tok::Local => self.local_statement()?,
            Tok::Return | Tok::Yield => {
                let is_return = self.token == Tok::Return;
                if !is_return {
                    self.f().generator = true;
                }
                self.advance()?;
                let value = match self.is_end_of_statement() {
                    true => None,
                    false => Some(self.comma_expr()?),
                };
                match is_return {
                    true => StmtKind::Return(value),
                    false => StmtKind::Yield,
                }
            }
            Tok::Break => {
                if self.f().breaks == 0 {
                    return self.error("'break' has to be in a loop block");
                }
                self.advance()?;
                StmtKind::Break
            }
            Tok::Continue => {
                if self.f().continues == 0 {
                    return self.error("'continue' has to be in a loop block");
                }
                self.advance()?;
                StmtKind::Continue
            }
            Tok::Function => self.function_statement()?,
            Tok::Class => self.class_statement()?,
            Tok::Enum => {
                self.enum_statement()?;
                StmtKind::Empty
            }
            Tok::Char(b'{') => {
                let size = self.f().top;
                self.advance()?;
                let body = self.statements()?;
                self.expect(Tok::Char(b'}'))?;
                self.f().set_top(size);
                StmtKind::Block(body)
            }
            Tok::Try => self.try_statement()?,
            Tok::Throw => {
                self.advance()?;
                StmtKind::Throw(self.comma_expr()?)
            }
            Tok::Const => {
                self.advance()?;
                let id = self.expect_ident()?;
                self.expect(Tok::Char(b'='))?;
                let value = self.expect_scalar()?;
                self.optional_semicolon()?;
                self.consts
                    .borrow_mut()
                    .table
                    .new_slot(Value::String(id), value);
                StmtKind::Empty
            }
            _ => StmtKind::Expr(self.comma_expr()?),
        };
        Ok(Stmt { line, kind })
    }

    fn if_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        self.expect(Tok::Char(b'('))?;
        let cond = self.comma_expr()?;
        self.expect(Tok::Char(b')'))?;
        let size = self.f().top;
        let then = self.statement()?;
        if self.token != Tok::Char(b'}') && self.token != Tok::Else {
            self.optional_semicolon()?;
        }
        self.f().set_top(size);
        let els = if self.token == Tok::Else {
            let size = self.f().top;
            self.advance()?;
            let els = self.statement()?;
            self.optional_semicolon()?;
            self.f().set_top(size);
            Some(Box::new(els))
        } else {
            None
        };
        Ok(StmtKind::If(cond, Box::new(then), els))
    }

    fn loop_body(&mut self) -> PResult<Stmt> {
        self.f().breaks += 1;
        self.f().continues += 1;
        let body = self.statement();
        self.f().breaks -= 1;
        self.f().continues -= 1;
        body
    }

    fn while_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        self.expect(Tok::Char(b'('))?;
        let cond = self.comma_expr()?;
        self.expect(Tok::Char(b')'))?;
        let size = self.f().top;
        let body = self.loop_body()?;
        self.f().set_top(size);
        Ok(StmtKind::While(cond, Box::new(body)))
    }

    fn do_while_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        let size = self.f().top;
        let body = self.loop_body()?;
        self.f().set_top(size);
        self.expect(Tok::While)?;
        self.expect(Tok::Char(b'('))?;
        let cond = self.comma_expr()?;
        self.expect(Tok::Char(b')'))?;
        Ok(StmtKind::DoWhile(Box::new(body), cond))
    }

    fn for_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        let size = self.f().top;
        self.expect(Tok::Char(b'('))?;
        let line = self.line();
        let init = if self.token == Tok::Local {
            Some(Box::new(Stmt {
                line,
                kind: self.local_statement()?,
            }))
        } else if self.token != Tok::Char(b';') {
            Some(Box::new(Stmt {
                line,
                kind: StmtKind::Expr(self.comma_expr()?),
            }))
        } else {
            None
        };
        self.expect(Tok::Char(b';'))?;
        let cond = match self.token != Tok::Char(b';') {
            true => Some(self.comma_expr()?),
            false => None,
        };
        self.expect(Tok::Char(b';'))?;
        let step = match self.token != Tok::Char(b')') {
            true => Some(self.comma_expr()?),
            false => None,
        };
        self.expect(Tok::Char(b')'))?;
        let body = self.loop_body()?;
        self.f().set_top(size);
        Ok(StmtKind::For {
            init,
            cond,
            step,
            body: Box::new(body),
        })
    }

    fn foreach_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        self.expect(Tok::Char(b'('))?;
        let mut value_name = self.expect_ident()?;
        let key_name = if self.token == Tok::Char(b',') {
            let key = value_name;
            self.advance()?;
            value_name = self.expect_ident()?;
            key
        } else {
            SqStr::from_str("@INDEX@")
        };
        self.expect(Tok::In)?;
        let size = self.f().top;
        let container = self.expression()?;
        self.expect(Tok::Char(b')'))?;
        let key_slot = self.f().push_local(key_name.clone());
        let value_slot = self.f().push_local(value_name.clone());
        // `@ITERATOR@` — the evaluator keeps the position itself, but the
        // slot is taken so that locals the body declares land where they
        // would in Squirrel.
        self.f().push_local(SqStr::from_str("@ITERATOR@"));
        let body = self.loop_body()?;
        self.f().set_top(size);
        Ok(StmtKind::Foreach {
            key: (key_name, key_slot),
            value: (value_name, value_slot),
            container,
            body: Box::new(body),
        })
    }

    fn switch_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        self.expect(Tok::Char(b'('))?;
        let value = self.comma_expr()?;
        self.expect(Tok::Char(b')'))?;
        self.expect(Tok::Char(b'{'))?;
        self.f().breaks += 1;
        let mut cases = Vec::new();
        while self.token == Tok::Case {
            self.advance()?;
            let test = self.expression()?;
            self.expect(Tok::Char(b':'))?;
            let size = self.f().top;
            let body = self.statements()?;
            self.f().set_top(size);
            cases.push((test, body));
        }
        let default = if self.token == Tok::Default {
            self.advance()?;
            self.expect(Tok::Char(b':'))?;
            let size = self.f().top;
            let body = self.statements()?;
            self.f().set_top(size);
            Some(body)
        } else {
            None
        };
        self.expect(Tok::Char(b'}'))?;
        self.f().breaks -= 1;
        Ok(StmtKind::Switch {
            value,
            cases,
            default,
        })
    }

    fn local_statement(&mut self) -> PResult<StmtKind> {
        let mut decls = Vec::new();
        loop {
            self.advance()?;
            let name = self.expect_ident()?;
            let init = if self.token == Tok::Char(b'=') {
                self.advance()?;
                Some(self.expression()?)
            } else {
                None
            };
            let slot = self.f().push_local(name.clone());
            decls.push((name, slot, init));
            if self.token != Tok::Char(b',') {
                break;
            }
        }
        Ok(StmtKind::Local(decls))
    }

    fn try_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        let body = self.statement()?;
        self.expect(Tok::Catch)?;
        self.expect(Tok::Char(b'('))?;
        let name = self.expect_ident()?;
        self.expect(Tok::Char(b')'))?;
        let size = self.f().top;
        let slot = self.f().push_local(name.clone());
        let handler = self.statement()?;
        self.f().set_top(size);
        Ok(StmtKind::Try {
            body: Box::new(body),
            catch: (name, slot),
            handler: Box::new(handler),
        })
    }

    /// `function a::b::c( ... ) body` — a new slot on `this`, or on whatever
    /// the `::` chain names.
    fn function_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        let mut id = self.expect_ident()?;
        let mut object = Expr::Local(0);
        while self.token == Tok::DoubleColon {
            object = Expr::Get(Box::new(object), Box::new(Expr::Str(id)));
            self.advance()?;
            id = self.expect_ident()?;
        }
        self.expect(Tok::Char(b'('))?;
        let proto = self.create_function(Value::String(id.clone()))?;
        Ok(StmtKind::Expr(Expr::NewSlot(
            Box::new(Expr::Get(Box::new(object), Box::new(Expr::Str(id)))),
            Box::new(Expr::Function(proto)),
        )))
    }

    fn class_statement(&mut self) -> PResult<StmtKind> {
        self.advance()?;
        let target = self.prefixed_expr()?;
        match target {
            Expr::Get(..) => {
                let class = self.class_expr()?;
                Ok(StmtKind::Expr(Expr::NewSlot(Box::new(target), Box::new(class))))
            }
            Expr::Local(_) | Expr::Outer(_) | Expr::Const(_) => {
                self.error("cannot create a class in a local with the syntax(class <local>)")
            }
            _ => self.error("invalid class name"),
        }
    }

    /// `SQCompiler::ExpectScalar`.
    fn expect_scalar(&mut self) -> PResult<Value> {
        let value = match self.token {
            Tok::Integer => Value::Integer(self.lex.nvalue),
            Tok::Float => Value::Float(self.lex.fvalue),
            Tok::StringLiteral => Value::String(self.lex.svalue.clone()),
            Tok::Char(b'-') => {
                self.advance()?;
                match self.token {
                    Tok::Integer => Value::Integer(self.lex.nvalue.wrapping_neg()),
                    Tok::Float => Value::Float(-self.lex.fvalue),
                    _ => return self.error("scalar expected : integer,float"),
                }
            }
            _ => return self.error("scalar expected : integer,float or string"),
        };
        self.advance()?;
        Ok(value)
    }

    fn enum_statement(&mut self) -> PResult<()> {
        self.advance()?;
        let id = self.expect_ident()?;
        self.expect(Tok::Char(b'{'))?;
        let table = Rc::new(std::cell::RefCell::new(TableObj::new(0)));
        let mut next_value = 0;
        while self.token != Tok::Char(b'}') {
            let key = self.expect_ident()?;
            let value = if self.token == Tok::Char(b'=') {
                self.advance()?;
                self.expect_scalar()?
            } else {
                let v = Value::Integer(next_value);
                next_value += 1;
                v
            };
            table.borrow_mut().table.new_slot(Value::String(key), value);
            if self.token == Tok::Char(b',') {
                self.advance()?;
            }
        }
        self.consts
            .borrow_mut()
            .table
            .new_slot(Value::String(id), Value::Table(table));
        self.advance()
    }

    // ----- expressions ---------------------------------------------------

    /// `SQCompiler::CommaExpr`.
    fn comma_expr(&mut self) -> PResult<Expr> {
        let first = self.expression()?;
        if self.token != Tok::Char(b',') {
            return Ok(first);
        }
        let mut all = vec![first];
        while self.token == Tok::Char(b',') {
            self.advance()?;
            all.push(self.expression()?);
        }
        Ok(Expr::Comma(all))
    }

    /// `SQCompiler::Expression` — the assignments and `?:`, over
    /// `LogicalOrExp`.
    fn expression(&mut self) -> PResult<Expr> {
        let lhs = self.logical_or()?;
        match self.token {
            Tok::Char(b'=')
            | Tok::NewSlot
            | Tok::MinusEq
            | Tok::PlusEq
            | Tok::MulEq
            | Tok::DivEq
            | Tok::ModEq => {
                let op = self.token;
                let deref = Deref::of(&lhs);
                if deref == Deref::None {
                    return self.error("can't assign expression");
                }
                self.advance()?;
                let rhs = self.expression()?;
                match op {
                    Tok::NewSlot => match deref {
                        Deref::Free => self.error("free variables cannot be modified"),
                        Deref::Field => Ok(Expr::NewSlot(Box::new(lhs), Box::new(rhs))),
                        _ => self.error("can't 'create' a local slot"),
                    },
                    Tok::Char(b'=') => match deref {
                        Deref::Free => self.error("free variables cannot be modified"),
                        _ => Ok(Expr::Assign(Box::new(lhs), Box::new(rhs))),
                    },
                    _ => {
                        let arith = match op {
                            Tok::MinusEq => ArithOp::Sub,
                            Tok::PlusEq => ArithOp::Add,
                            Tok::MulEq => ArithOp::Mul,
                            Tok::DivEq => ArithOp::Div,
                            _ => ArithOp::Mod,
                        };
                        Ok(Expr::Compound(arith, Box::new(lhs), Box::new(rhs)))
                    }
                }
            }
            Tok::Char(b'?') => {
                self.advance()?;
                let a = self.expression()?;
                self.expect(Tok::Char(b':'))?;
                let b = self.expression()?;
                Ok(Expr::Ternary(Box::new(lhs), Box::new(a), Box::new(b)))
            }
            _ => Ok(lhs),
        }
    }

    fn logical_or(&mut self) -> PResult<Expr> {
        let lhs = self.logical_and()?;
        if self.token == Tok::Or {
            self.advance()?;
            let rhs = self.logical_or()?;
            return Ok(Expr::Or(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn logical_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitwise_or()?;
        loop {
            match self.token {
                Tok::And => {
                    self.advance()?;
                    let rhs = self.logical_and()?;
                    lhs = Expr::And(Box::new(lhs), Box::new(rhs));
                }
                Tok::In => {
                    self.advance()?;
                    let rhs = self.bitwise_or()?;
                    lhs = Expr::Binary(BinOp::In, Box::new(lhs), Box::new(rhs));
                }
                Tok::Instanceof => {
                    self.advance()?;
                    let rhs = self.bitwise_or()?;
                    lhs = Expr::Binary(BinOp::InstanceOf, Box::new(lhs), Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn binary_level(
        &mut self,
        next: fn(&mut Self) -> PResult<Expr>,
        op_of: fn(Tok) -> Option<BinOp>,
    ) -> PResult<Expr> {
        let mut lhs = next(self)?;
        while let Some(op) = op_of(self.token) {
            self.advance()?;
            let rhs = next(self)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn bitwise_or(&mut self) -> PResult<Expr> {
        self.binary_level(Self::bitwise_xor, |t| {
            (t == Tok::Char(b'|')).then_some(BinOp::Bit(BitOp::Or))
        })
    }

    fn bitwise_xor(&mut self) -> PResult<Expr> {
        self.binary_level(Self::bitwise_and, |t| {
            (t == Tok::Char(b'^')).then_some(BinOp::Bit(BitOp::Xor))
        })
    }

    fn bitwise_and(&mut self) -> PResult<Expr> {
        self.binary_level(Self::comparison, |t| {
            (t == Tok::Char(b'&')).then_some(BinOp::Bit(BitOp::And))
        })
    }

    fn comparison(&mut self) -> PResult<Expr> {
        self.binary_level(Self::shift, |t| match t {
            Tok::Eq => Some(BinOp::Eq),
            Tok::Char(b'>') => Some(BinOp::Cmp(CmpOp::Greater)),
            Tok::Char(b'<') => Some(BinOp::Cmp(CmpOp::Less)),
            Tok::Ge => Some(BinOp::Cmp(CmpOp::GreaterEq)),
            Tok::Le => Some(BinOp::Cmp(CmpOp::LessEq)),
            Tok::Ne => Some(BinOp::Ne),
            _ => None,
        })
    }

    fn shift(&mut self) -> PResult<Expr> {
        self.binary_level(Self::plus, |t| match t {
            Tok::UShiftR => Some(BinOp::Bit(BitOp::UShiftR)),
            Tok::ShiftL => Some(BinOp::Bit(BitOp::ShiftL)),
            Tok::ShiftR => Some(BinOp::Bit(BitOp::ShiftR)),
            _ => None,
        })
    }

    fn plus(&mut self) -> PResult<Expr> {
        self.binary_level(Self::mult, |t| match t {
            Tok::Char(b'+') => Some(BinOp::Arith(ArithOp::Add)),
            Tok::Char(b'-') => Some(BinOp::Arith(ArithOp::Sub)),
            _ => None,
        })
    }

    fn mult(&mut self) -> PResult<Expr> {
        self.binary_level(Self::prefixed_expr, |t| match t {
            Tok::Char(b'*') => Some(BinOp::Arith(ArithOp::Mul)),
            Tok::Char(b'/') => Some(BinOp::Arith(ArithOp::Div)),
            Tok::Char(b'%') => Some(BinOp::Arith(ArithOp::Mod)),
            _ => None,
        })
    }

    /// `SQCompiler::PrefixedExpr` — a factor and its `.`, `[]`, `()` and
    /// postfix `++`/`--`.
    fn prefixed_expr(&mut self) -> PResult<Expr> {
        let mut expr = self.factor()?;
        loop {
            match self.token {
                Tok::Char(b'.') => {
                    self.advance()?;
                    if self.token == Tok::Parent {
                        self.advance()?;
                        if matches!(
                            self.token,
                            Tok::Char(b'=')
                                | Tok::Char(b'(')
                                | Tok::NewSlot
                                | Tok::PlusPlus
                                | Tok::MinusMinus
                                | Tok::PlusEq
                                | Tok::MinusEq
                                | Tok::MulEq
                                | Tok::DivEq
                                | Tok::ModEq
                        ) {
                            return self.error("parent cannot be set");
                        }
                        expr = Expr::Parent(Box::new(expr));
                    } else {
                        let key = self.expect_ident()?;
                        expr = Expr::Get(Box::new(expr), Box::new(Expr::Str(key)));
                    }
                }
                Tok::Char(b'[') => {
                    if self.lex.prev_token == Prev::Newline {
                        return self.error(
                            "cannot brake deref/or comma needed after [exp]=exp slot declaration",
                        );
                    }
                    self.advance()?;
                    let key = self.expression()?;
                    self.expect(Tok::Char(b']'))?;
                    expr = Expr::Get(Box::new(expr), Box::new(key));
                }
                Tok::PlusPlus | Tok::MinusMinus => {
                    if Deref::of(&expr) != Deref::None && !self.is_end_of_statement() {
                        let delta = if self.token == Tok::MinusMinus { -1 } else { 1 };
                        self.advance()?;
                        return Ok(Expr::PostIncr(Box::new(expr), delta));
                    }
                    return Ok(expr);
                }
                Tok::Char(b'(') => {
                    self.advance()?;
                    let args = self.call_args()?;
                    expr = Expr::Call(Box::new(expr), args);
                }
                _ => return Ok(expr),
            }
        }
    }

    /// `SQCompiler::FunctionCallArgs` — no separator is required between two
    /// arguments, and a trailing comma is an error.
    fn call_args(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        while self.token != Tok::Char(b')') {
            args.push(self.expression()?);
            if self.token == Tok::Char(b',') {
                self.advance()?;
                if self.token == Tok::Char(b')') {
                    return self.error("expression expected, found ')'");
                }
            }
            if self.token == Tok::Eob {
                return self.expected(Tok::Char(b')'));
            }
        }
        self.advance()?;
        Ok(args)
    }

    fn factor(&mut self) -> PResult<Expr> {
        match self.token {
            Tok::StringLiteral => {
                let s = self.lex.svalue.clone();
                self.advance()?;
                Ok(Expr::Str(s))
            }
            Tok::Vargc => {
                self.advance()?;
                Ok(Expr::Vargc)
            }
            Tok::Vargv => {
                self.advance()?;
                self.expect(Tok::Char(b'['))?;
                let index = self.expression()?;
                self.expect(Tok::Char(b']'))?;
                Ok(Expr::Vargv(Box::new(index)))
            }
            Tok::Identifier | Tok::Constructor | Tok::This => {
                let id = match self.token {
                    Tok::Identifier => self.lex.svalue.clone(),
                    Tok::This => SqStr::from_str("this"),
                    _ => SqStr::from_str("constructor"),
                };
                self.advance()?;
                self.resolve(id)
            }
            Tok::Parent => {
                self.advance()?;
                Ok(Expr::Parent(Box::new(Expr::Local(0))))
            }
            Tok::DoubleColon => {
                // "hack": the token becomes `.`, so the root table is
                // indexed by whatever identifier follows.
                self.token = Tok::Char(b'.');
                Ok(Expr::Root)
            }
            Tok::Null => {
                self.advance()?;
                Ok(Expr::Null)
            }
            Tok::Integer => {
                let v = self.lex.nvalue;
                self.advance()?;
                Ok(Expr::Int(v))
            }
            Tok::Float => {
                let v = self.lex.fvalue;
                self.advance()?;
                Ok(Expr::Float(v))
            }
            Tok::True | Tok::False => {
                let v = self.token == Tok::True;
                self.advance()?;
                Ok(Expr::Bool(v))
            }
            Tok::Char(b'[') => {
                self.advance()?;
                let mut items = Vec::new();
                while self.token != Tok::Char(b']') {
                    if self.token == Tok::Eob {
                        return self.expected(Tok::Char(b']'));
                    }
                    items.push(self.expression()?);
                    if self.token == Tok::Char(b',') {
                        self.advance()?;
                    }
                }
                self.advance()?;
                Ok(Expr::Array(items))
            }
            Tok::Char(b'{') => {
                self.advance()?;
                let slots = self.table_body(Tok::Char(b','), Tok::Char(b'}'))?;
                Ok(Expr::Table(slots.into_iter().map(|m| (m.key, m.value)).collect()))
            }
            Tok::Function => {
                self.advance()?;
                self.expect(Tok::Char(b'('))?;
                let proto = self.create_function(Value::Null)?;
                Ok(Expr::Function(proto))
            }
            Tok::Class => {
                self.advance()?;
                self.class_expr()
            }
            Tok::Char(b'-') => self.unary(UnaryOp::Neg),
            Tok::Char(b'!') => self.unary(UnaryOp::Not),
            Tok::Char(b'~') => self.unary(UnaryOp::BitNot),
            Tok::Typeof => self.unary(UnaryOp::Typeof),
            Tok::Resume => self.unary(UnaryOp::Resume),
            Tok::Clone => self.unary(UnaryOp::Clone),
            Tok::MinusMinus | Tok::PlusPlus => {
                let delta = if self.token == Tok::MinusMinus { -1 } else { 1 };
                self.advance()?;
                let target = self.prefixed_expr()?;
                Ok(Expr::PreIncr(Box::new(target), delta))
            }
            Tok::Delete => {
                self.advance()?;
                let target = self.prefixed_expr()?;
                match Deref::of(&target) {
                    Deref::Field => Ok(Expr::Delete(Box::new(target))),
                    Deref::None => self.error("can't delete an expression"),
                    _ => self.error("cannot delete a local"),
                }
            }
            Tok::Delegate => {
                self.advance()?;
                let delegate = self.comma_expr()?;
                self.expect(Tok::Char(b':'))?;
                let table = self.comma_expr()?;
                Ok(Expr::Delegate(Box::new(delegate), Box::new(table)))
            }
            Tok::Char(b'(') => {
                self.advance()?;
                let inner = self.comma_expr()?;
                self.expect(Tok::Char(b')'))?;
                Ok(Expr::Group(Box::new(inner)))
            }
            _ => self.error("expression expected"),
        }
    }

    fn unary(&mut self, op: UnaryOp) -> PResult<Expr> {
        self.advance()?;
        let operand = self.prefixed_expr()?;
        Ok(Expr::Unary(op, Box::new(operand)))
    }

    /// `Factor`'s identifier resolution: local, outer, constant, field of
    /// `this` — in that order, and at compile time.
    fn resolve(&mut self, id: SqStr) -> PResult<Expr> {
        if let Some(slot) = self.f().local(&id) {
            return Ok(Expr::Local(slot));
        }
        if let Some(index) = self.f().outer(&id) {
            return Ok(Expr::Outer(index));
        }
        let constant = self.consts.borrow().table.get(&Value::String(id.clone()));
        if let Some(constant) = constant {
            let value = match constant {
                Value::Table(enumeration) => {
                    self.expect(Tok::Char(b'.'))?;
                    let member = self.expect_ident()?;
                    let found = enumeration.borrow().table.get(&Value::String(member.clone()));
                    match found {
                        Some(v) => v,
                        None => {
                            return self.error(format!(
                                "invalid constant [{}.{}]",
                                id.to_string_lossy(),
                                member.to_string_lossy()
                            ))
                        }
                    }
                }
                other => other,
            };
            return Ok(Expr::Const(value));
        }
        Ok(Expr::Get(Box::new(Expr::Local(0)), Box::new(Expr::Str(id))))
    }

    /// `ParseTableOrClass` — a table literal's slots (separator `,`) or a
    /// class body's members (separator `;`, with attributes and `static`).
    fn table_body(&mut self, separator: Tok, terminator: Tok) -> PResult<Vec<ClassMember>> {
        let mut members = Vec::new();
        while self.token != terminator {
            if self.token == Tok::Eob {
                return self.expected(terminator);
            }
            let mut attributes = None;
            let mut is_static = false;
            if separator == Tok::Char(b';') {
                if self.token == Tok::AttrOpen {
                    self.advance()?;
                    let attrs = self.table_body(Tok::Char(b','), Tok::AttrClose)?;
                    attributes = Some(Expr::Table(
                        attrs.into_iter().map(|m| (m.key, m.value)).collect(),
                    ));
                }
                if self.token == Tok::Static {
                    is_static = true;
                    self.advance()?;
                }
            }
            let (key, value) = match self.token {
                Tok::Function | Tok::Constructor => {
                    let is_function = self.token == Tok::Function;
                    self.advance()?;
                    let id = match is_function {
                        true => self.expect_ident()?,
                        false => SqStr::from_str("constructor"),
                    };
                    self.expect(Tok::Char(b'('))?;
                    let proto = self.create_function(Value::String(id.clone()))?;
                    (Expr::Str(id), Expr::Function(proto))
                }
                Tok::Char(b'[') => {
                    self.advance()?;
                    let key = self.comma_expr()?;
                    self.expect(Tok::Char(b']'))?;
                    self.expect(Tok::Char(b'='))?;
                    (key, self.expression()?)
                }
                _ => {
                    let id = self.expect_ident()?;
                    self.expect(Tok::Char(b'='))?;
                    (Expr::Str(id), self.expression()?)
                }
            };
            if self.token == separator {
                self.advance()?;
            }
            members.push(ClassMember {
                attributes,
                is_static,
                key,
                value,
            });
        }
        self.advance()?;
        Ok(members)
    }

    /// `SQCompiler::ClassExp` — after `class` (and a name, for a statement).
    fn class_expr(&mut self) -> PResult<Expr> {
        let base = if self.token == Tok::Extends {
            self.advance()?;
            Some(self.expression()?)
        } else {
            None
        };
        let attributes = if self.token == Tok::AttrOpen {
            self.advance()?;
            let attrs = self.table_body(Tok::Char(b','), Tok::AttrClose)?;
            Some(Expr::Table(attrs.into_iter().map(|m| (m.key, m.value)).collect()))
        } else {
            None
        };
        self.expect(Tok::Char(b'{'))?;
        let members = self.table_body(Tok::Char(b';'), Tok::Char(b'}'))?;
        Ok(Expr::Class(Box::new(ClassExpr {
            base,
            attributes,
            members,
        })))
    }

    /// `SQCompiler::CreateFunction` — after the `(`.
    fn create_function(&mut self, name: Value) -> PResult<Rc<FuncProto>> {
        let line = self.line();
        let mut child = FuncState::new();
        child.push_local(SqStr::from_str("this"));
        let mut params = vec![SqStr::from_str("this")];
        let mut default_exprs = Vec::new();
        let mut varparams = false;
        while self.token != Tok::Char(b')') {
            if self.token == Tok::VarParams {
                if !default_exprs.is_empty() {
                    return self.error(
                        "function with default parameters cannot have variable number of parameters",
                    );
                }
                varparams = true;
                self.advance()?;
                if self.token != Tok::Char(b')') {
                    return self.error("expected ')'");
                }
                break;
            }
            let param = self.expect_ident()?;
            child.push_local(param.clone());
            params.push(param);
            if self.token == Tok::Char(b'=') {
                self.advance()?;
                // Compiled in the *enclosing* function, which is where the
                // closure is made.
                default_exprs.push(self.expression()?);
            } else if !default_exprs.is_empty() {
                return self.error("expected '='");
            }
            if self.token == Tok::Char(b',') {
                self.advance()?;
            } else if self.token != Tok::Char(b')') {
                return self.error("expected ')' or ','");
            }
        }
        self.expect(Tok::Char(b')'))?;

        let mut outers = Vec::new();
        if self.token == Tok::Char(b':') {
            self.advance()?;
            self.expect(Tok::Char(b'('))?;
            while self.token != Tok::Char(b')') {
                let name = self.expect_ident()?;
                // `SQFuncState::AddOuterValue` — resolved against the parent.
                let parent = self.fs.last().expect("a parent function");
                let kind = if let Some(slot) = parent.local(&name) {
                    Outer::Local(slot)
                } else if let Some(index) = parent.outer(&name) {
                    Outer::Outer(index)
                } else {
                    Outer::Symbol(name.clone())
                };
                child.outers.push((name.clone(), kind.clone()));
                outers.push(kind);
                if self.token == Tok::Char(b',') {
                    self.advance()?;
                } else if self.token != Tok::Char(b')') {
                    return self.error("expected ')' or ','");
                }
            }
            self.advance()?;
        }

        self.fs.push(child);
        let body = self.statement();
        let child = self.fs.pop().expect("the child function's state");
        let body = body?;
        Ok(Rc::new(FuncProto {
            name,
            source: self.source.clone(),
            n_defaults: default_exprs.len(),
            default_exprs,
            params,
            varparams,
            outers,
            stack_size: child.max as usize,
            body,
            generator: child.generator,
            line,
        }))
    }
}

/// What `_exst._deref` would say about an expression: whether it names a
/// field, a local, a copy (an outer or a constant — `_freevar`), or nothing
/// assignable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Deref {
    None,
    Field,
    Local,
    Free,
}

impl Deref {
    pub(super) fn of(expr: &Expr) -> Deref {
        match expr {
            Expr::Get(..) => Deref::Field,
            Expr::Local(_) => Deref::Local,
            Expr::Outer(_) | Expr::Const(_) => Deref::Free,
            _ => Deref::None,
        }
    }
}
