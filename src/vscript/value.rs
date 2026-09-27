//! `SQObjectPtr` and the objects it points at (`sqobject.h`, `sqclass.h`,
//! `sqclosure.h`, `sqarray.h`).
//!
//! Reference counting is `Rc`, which is what Squirrel's `_uiRef` is. What `Rc`
//! cannot do is collect a cycle, and neither can Squirrel without its
//! collector — Valve disabled the per-frame collection (`vsquirrel.cpp`'s
//! `Frame`, "our scripts are supposed to never create circular references")
//! and relies on `sq_close` finalising every object at level end. The port
//! does the same: [`Vm`](super::Vm) keeps a weak list of every container it
//! made and empties them when it is dropped.

use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

use super::ast::FuncProto;
use super::table::Table;
use super::Vm;

/// An interned-by-content string: `SQString`. Squirrel strings are **bytes**,
/// not text — a `.nut` file is read as `char *` — so this is not a `String`.
#[derive(Clone)]
pub struct SqStr(Rc<StrData>);

struct StrData {
    bytes: Box<[u8]>,
    hash: u32,
}

/// `_hashstr` (`sqstring.h:4`), over **signed** `char`s widened to
/// `unsigned short`, which is what `(unsigned short)*(s++)` does to a byte
/// above 127 on a compiler whose `char` is signed.
pub fn hash_bytes(s: &[u8]) -> u32 {
    let mut l = s.len();
    let mut h = l as u32;
    let step = (l >> 5) | 1;
    let mut i = 0;
    while l >= step {
        let c = s[i] as i8 as i16 as u16 as u32;
        h ^= (h << 5).wrapping_add(h >> 2).wrapping_add(c);
        i += 1;
        l -= step;
    }
    h
}

impl SqStr {
    pub fn new(bytes: &[u8]) -> SqStr {
        SqStr(Rc::new(StrData {
            hash: hash_bytes(bytes),
            bytes: bytes.into(),
        }))
    }

    pub fn from_str(text: &str) -> SqStr {
        SqStr::new(text.as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0.bytes
    }

    pub fn len(&self) -> usize {
        self.0.bytes.len()
    }

    pub fn hash(&self) -> u32 {
        self.0.hash
    }

    /// For printing and for handing to the rest of the engine. Bytes that are
    /// not UTF-8 come out as U+FFFD; nothing a script *compares* goes through
    /// here.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.0.bytes).into_owned()
    }

    fn ptr(&self) -> usize {
        Rc::as_ptr(&self.0) as *const u8 as usize
    }
}

impl PartialEq for SqStr {
    fn eq(&self, other: &SqStr) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || (self.hash() == other.hash() && self.as_bytes() == other.as_bytes())
    }
}

impl std::fmt::Debug for SqStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.to_string_lossy())
    }
}

pub type TableRef = Rc<RefCell<TableObj>>;
pub type ArrayRef = Rc<RefCell<Vec<Value>>>;
pub type ClassRef = Rc<RefCell<Class>>;
pub type InstanceRef = Rc<RefCell<Instance>>;

/// A table object: `SQTable` with its `SQDelegable::_delegate`.
pub struct TableObj {
    pub table: Table,
    pub delegate: Option<TableRef>,
}

impl TableObj {
    pub fn new(initial_size: usize) -> TableObj {
        TableObj {
            table: Table::new(initial_size),
            delegate: None,
        }
    }
}

/// A script closure: `SQClosure`.
pub struct Closure {
    pub proto: Rc<FuncProto>,
    /// `_outervalues` — copies taken when the closure was made. Squirrel 2 has
    /// no upvalues: a function sees an enclosing local only if it lists it in
    /// `function(...):(x)`, and then it sees the value it had at that moment.
    pub outers: Vec<Value>,
    /// `_defaultparams`, evaluated when the closure was made.
    pub defaults: Vec<Value>,
    /// `_env` — the `this` a `bindenv` pinned, held weakly as Squirrel does.
    pub env: Option<WeakRef>,
}

/// A native function: `SQNativeClosure`. `args[0]` is `this`. An `Err` of
/// `Value::Null` is `return SQ_ERROR` with no message of its own.
pub type NativeFn = dyn Fn(&mut Vm, &mut dyn Any, &[Value]) -> Result<Value, Value>;

pub struct Native {
    pub name: SqStr,
    pub func: Rc<NativeFn>,
    /// `_nparamscheck`: positive is an exact count, negative a minimum, zero
    /// no check. Counts include `this`.
    pub nparamscheck: i32,
    /// `_typecheck`, one mask per parameter from `this` on; `u32::MAX` is
    /// "anything".
    pub typecheck: Vec<u32>,
    pub env: Option<WeakRef>,
}

/// `SQClassMember`.
#[derive(Clone)]
pub struct Member {
    pub val: Value,
    pub attrs: Value,
}

/// `MEMBER_TYPE_METHOD` and `MEMBER_TYPE_FIELD` (`sqclass.h:21`), which is how
/// a class's member table says which of its two vectors a name indexes.
pub const MEMBER_METHOD: i32 = 0x0100_0000;
pub const MEMBER_FIELD: i32 = 0x0200_0000;

/// `SQClass`.
pub struct Class {
    pub members: Table,
    pub base: Option<ClassRef>,
    pub default_values: Vec<Member>,
    pub methods: Vec<Member>,
    pub metamethods: Vec<Value>,
    pub attributes: Value,
    pub locked: bool,
    /// `_typetag` — how native code recognises its own classes.
    pub type_tag: usize,
}

/// `SQInstance`.
pub struct Instance {
    pub class: ClassRef,
    pub values: Vec<Value>,
    /// `_userpointer` — what a native class hangs off its instances.
    pub user: Option<Box<dyn Any>>,
}

/// A weak reference: `SQWeakRef`. Reads as `null` once its object is gone.
#[derive(Clone)]
pub enum WeakRef {
    Table(Weak<RefCell<TableObj>>),
    Array(Weak<RefCell<Vec<Value>>>),
    Closure(Weak<Closure>),
    Native(Weak<Native>),
    Class(Weak<RefCell<Class>>),
    Instance(Weak<RefCell<Instance>>),
    /// Strings are reference counted in Squirrel too, but one that is still
    /// named by a weak reference is also still in the interned string table
    /// that made it, so it never dies while anything can ask.
    String(SqStr),
}

impl WeakRef {
    pub fn get(&self) -> Value {
        match self {
            WeakRef::Table(w) => w.upgrade().map_or(Value::Null, Value::Table),
            WeakRef::Array(w) => w.upgrade().map_or(Value::Null, Value::Array),
            WeakRef::Closure(w) => w.upgrade().map_or(Value::Null, Value::Closure),
            WeakRef::Native(w) => w.upgrade().map_or(Value::Null, Value::Native),
            WeakRef::Class(w) => w.upgrade().map_or(Value::Null, Value::Class),
            WeakRef::Instance(w) => w.upgrade().map_or(Value::Null, Value::Instance),
            WeakRef::String(s) => Value::String(s.clone()),
        }
    }

    fn address(&self) -> usize {
        match self {
            WeakRef::Table(w) => w.as_ptr() as *const u8 as usize,
            WeakRef::Array(w) => w.as_ptr() as *const u8 as usize,
            WeakRef::Closure(w) => w.as_ptr() as *const u8 as usize,
            WeakRef::Native(w) => w.as_ptr() as *const u8 as usize,
            WeakRef::Class(w) => w.as_ptr() as *const u8 as usize,
            WeakRef::Instance(w) => w.as_ptr() as *const u8 as usize,
            WeakRef::String(s) => s.ptr(),
        }
    }
}

/// `SQObjectPtr`.
#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    /// `SQInteger`: 32 bits, because Portal 2 shipped a 32-bit build and
    /// `_SQ64` is defined only for 64-bit ones (`squirrel.h`). Arithmetic
    /// wraps, which is what the C does in practice.
    Integer(i32),
    /// `SQFloat`: single precision for the same reason — `SQUSEDOUBLE` comes
    /// with `_SQ64`.
    Float(f32),
    String(SqStr),
    Table(TableRef),
    Array(ArrayRef),
    Closure(Rc<Closure>),
    Native(Rc<Native>),
    Class(ClassRef),
    Instance(InstanceRef),
    WeakRef(Rc<WeakRef>),
}

/// `_RT_*` (`squirrel.h`), which are also the bits a native's type mask is
/// built from.
pub mod rt {
    pub const NULL: u32 = 0x0000_0001;
    pub const INTEGER: u32 = 0x0000_0002;
    pub const FLOAT: u32 = 0x0000_0004;
    pub const BOOL: u32 = 0x0000_0008;
    pub const STRING: u32 = 0x0000_0010;
    pub const TABLE: u32 = 0x0000_0020;
    pub const ARRAY: u32 = 0x0000_0040;
    pub const USERDATA: u32 = 0x0000_0080;
    pub const CLOSURE: u32 = 0x0000_0100;
    pub const NATIVECLOSURE: u32 = 0x0000_0200;
    pub const GENERATOR: u32 = 0x0000_0400;
    pub const USERPOINTER: u32 = 0x0000_0800;
    pub const THREAD: u32 = 0x0000_1000;
    pub const FUNCPROTO: u32 = 0x0000_2000;
    pub const CLASS: u32 = 0x0000_4000;
    pub const INSTANCE: u32 = 0x0000_8000;
    pub const WEAKREF: u32 = 0x0001_0000;
}

/// `IdType2Name` (`sqobject.cpp:19`).
pub fn type_name_of_bit(bit: u32) -> &'static str {
    match bit {
        rt::NULL => "null",
        rt::INTEGER => "integer",
        rt::FLOAT => "float",
        rt::BOOL => "bool",
        rt::STRING => "string",
        rt::TABLE => "table",
        rt::ARRAY => "array",
        rt::GENERATOR => "generator",
        rt::CLOSURE | rt::FUNCPROTO => "function",
        rt::NATIVECLOSURE => "native function",
        rt::USERDATA | rt::USERPOINTER => "userdata",
        rt::THREAD => "thread",
        rt::CLASS => "class",
        rt::INSTANCE => "instance",
        rt::WEAKREF => "weakref",
        _ => "",
    }
}

impl Value {
    pub fn str(text: &str) -> Value {
        Value::String(SqStr::from_str(text))
    }

    pub fn bytes(bytes: &[u8]) -> Value {
        Value::String(SqStr::new(bytes))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The `_RT_*` bit of this value's type.
    pub fn type_bit(&self) -> u32 {
        match self {
            Value::Null => rt::NULL,
            Value::Bool(_) => rt::BOOL,
            Value::Integer(_) => rt::INTEGER,
            Value::Float(_) => rt::FLOAT,
            Value::String(_) => rt::STRING,
            Value::Table(_) => rt::TABLE,
            Value::Array(_) => rt::ARRAY,
            Value::Closure(_) => rt::CLOSURE,
            Value::Native(_) => rt::NATIVECLOSURE,
            Value::Class(_) => rt::CLASS,
            Value::Instance(_) => rt::INSTANCE,
            Value::WeakRef(_) => rt::WEAKREF,
        }
    }

    /// `GetTypeName` — what `type()` returns and what errors print.
    pub fn type_name(&self) -> &'static str {
        type_name_of_bit(self.type_bit())
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Integer(_) | Value::Float(_))
    }

    /// `tofloat`.
    pub fn to_float(&self) -> f32 {
        match self {
            Value::Integer(i) => *i as f32,
            Value::Float(f) => *f,
            _ => 0.0,
        }
    }

    /// `tointeger` — truncation, as the C cast does.
    pub fn to_integer(&self) -> i32 {
        match self {
            Value::Integer(i) => *i,
            Value::Float(f) => float_to_int(*f),
            _ => 0,
        }
    }

    /// `SQVM::IsFalse`: null, `false`, 0 and 0.0.
    pub fn is_false(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Bool(b) => !b,
            Value::Integer(i) => *i == 0,
            Value::Float(f) => *f == 0.0,
            _ => false,
        }
    }

    /// `_realval` — a weak reference read through, anything else as is.
    pub fn real(&self) -> Value {
        match self {
            Value::WeakRef(w) => w.get(),
            other => other.clone(),
        }
    }

    /// The object's address, for `hashptr` and for printing `0x%p`. Zero for
    /// the value types.
    pub fn address(&self) -> usize {
        match self {
            Value::Null | Value::Bool(_) | Value::Integer(_) | Value::Float(_) => 0,
            Value::String(s) => s.ptr(),
            Value::Table(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Array(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Closure(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Native(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Class(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Instance(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::WeakRef(r) => Rc::as_ptr(r) as *const u8 as usize,
        }
    }

    /// A plain rendering, for tests and diagnostics — not `tostring()`, which
    /// is [`Vm::to_string`](super::Vm::to_string) and can run a metamethod.
    pub fn to_display_string(&self) -> String {
        match self {
            Value::String(s) => s.to_string_lossy(),
            Value::Integer(i) => i.to_string(),
            Value::Float(f) => super::format::format_g(*f as f64, 6, false),
            Value::Bool(b) => b.to_string(),
            other => format!("({} : 0x{:08X})", other.type_name(), other.address() as u32),
        }
    }

    pub fn as_string(&self) -> Option<&SqStr> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// A weak reference to this value, or the value itself if it has no
    /// reference count — `sq_weakref`.
    pub fn weak(&self) -> Value {
        let weak = match self {
            Value::Table(r) => WeakRef::Table(Rc::downgrade(r)),
            Value::Array(r) => WeakRef::Array(Rc::downgrade(r)),
            Value::Closure(r) => WeakRef::Closure(Rc::downgrade(r)),
            Value::Native(r) => WeakRef::Native(Rc::downgrade(r)),
            Value::Class(r) => WeakRef::Class(Rc::downgrade(r)),
            Value::Instance(r) => WeakRef::Instance(Rc::downgrade(r)),
            Value::String(s) => WeakRef::String(s.clone()),
            Value::WeakRef(_) => return self.clone(),
            _ => return self.clone(),
        };
        Value::WeakRef(Rc::new(weak))
    }
}

/// `(SQInteger)f` as x86 computes it: truncation, and `0x80000000` — the
/// "integer indefinite" `cvttss2si` returns — for NaN or anything out of
/// range, where Rust's `as` would saturate.
pub fn float_to_int(f: f32) -> i32 {
    if f.is_nan() || f >= 2_147_483_648.0 || f < -2_147_483_648.0 {
        i32::MIN
    } else {
        f as i32
    }
}

/// `_rawval(a) == _rawval(b) && type(a) == type(b)` — what a table compares
/// keys with and what `==` is between two values of one type. Strings compare
/// by content, which is what pointer equality means for an interned string;
/// floats by **bits**, so `0.0 != -0.0` and a NaN equals itself; objects by
/// identity. Two weak references are equal when they point at one object,
/// which is what Squirrel's one-weakref-per-object cache makes pointer
/// equality mean.
pub fn raw_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Integer(x), Value::Integer(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::WeakRef(x), Value::WeakRef(y)) => x.address() == y.address(),
        (x, y) if x.type_bit() == y.type_bit() => x.address() == y.address(),
        _ => false,
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{s:?}"),
            other => write!(f, "{}", other.to_display_string()),
        }
    }
}
