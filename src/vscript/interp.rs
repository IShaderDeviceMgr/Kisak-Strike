//! `SQVM` (`sqvm.cpp`) as a tree walker.
//!
//! Each function here names the `SQVM` member it is. The ones worth reading
//! before changing anything:
//!
//! - [`Vm::get_value`] is `SQVM::Get` + `FallBackGet`: own slots, then the
//!   delegate chain, then `_get`, then the type's default delegate — and last,
//!   **the root table, but only when the object is the running function's own
//!   `this`**. That one rule is what makes an unqualified name in an entity's
//!   script find a global.
//! - [`Vm::raise`] is `exception_trap`'s decision: an error goes to the error
//!   handler at the moment it is raised, and only if the current
//!   [`Execute`](super::Execute) has no `try` open. That is Squirrel's
//!   behaviour, including printing an error that an outer `try` then catches
//!   when a native call sits between them.

use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

use super::ast::*;
use super::format::format_g;
use super::value::*;
use super::{Execute, Finalize, Frame, MetaMethod, Vm, MAX_NATIVE_CALLS, MAX_SCRIPT_DEPTH};

pub(super) enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
}

type R<T> = Result<T, Value>;

fn err(message: impl AsRef<str>) -> Value {
    Value::str(message.as_ref())
}

/// `SQClass::SQClass`.
pub(super) fn create_class(base: Option<ClassRef>) -> ClassRef {
    let class = match &base {
        Some(base) => {
            let b = base.borrow();
            Class {
                members: b.members.clone_table(),
                base: Some(base.clone()),
                default_values: b.default_values.clone(),
                methods: b.methods.clone(),
                metamethods: b.metamethods.clone(),
                attributes: Value::Null,
                locked: false,
                type_tag: 0,
            }
        }
        None => Class {
            members: super::table::Table::new(0),
            base: None,
            default_values: Vec::new(),
            methods: Vec::new(),
            metamethods: vec![Value::Null; super::METAMETHODS.len()],
            attributes: Value::Null,
            locked: false,
            type_tag: 0,
        },
    };
    Rc::new(RefCell::new(class))
}

/// `SQClass::NewSlot`. Returns false only when the class is locked.
pub(super) fn class_new_slot(class: &ClassRef, key: Value, val: Value, is_static: bool) -> bool {
    let mut c = class.borrow_mut();
    if c.locked {
        return false;
    }
    let existing = c.members.get(&key);
    if let Some(Value::Integer(idx)) = &existing {
        if idx & MEMBER_FIELD != 0 {
            // Overrides the default value.
            let i = (idx & 0x00FF_FFFF) as usize;
            c.default_values[i].val = val;
            return true;
        }
    }
    let callable = matches!(val, Value::Closure(_) | Value::Native(_));
    if callable || is_static {
        let mm = match (callable, &key) {
            (true, Value::String(name)) => super::METAMETHODS
                .iter()
                .position(|m| m.as_bytes() == name.as_bytes()),
            _ => None,
        };
        if let Some(mm) = mm {
            c.metamethods[mm] = val;
        } else if let Some(Value::Integer(idx)) = existing {
            let i = (idx & 0x00FF_FFFF) as usize;
            c.methods[i].val = val;
        } else {
            let index = c.methods.len() as i32;
            c.members.new_slot(key, Value::Integer(MEMBER_METHOD | index));
            c.methods.push(Member {
                val,
                attrs: Value::Null,
            });
        }
        return true;
    }
    let index = c.default_values.len() as i32;
    c.members.new_slot(key, Value::Integer(MEMBER_FIELD | index));
    c.default_values.push(Member {
        val,
        attrs: Value::Null,
    });
    true
}

/// `SQClass::Get`.
pub(super) fn class_get(class: &ClassRef, key: &Value) -> Option<Value> {
    let c = class.borrow();
    match c.members.get(key)? {
        Value::Integer(idx) if idx & MEMBER_FIELD != 0 => {
            Some(c.default_values[(idx & 0x00FF_FFFF) as usize].val.real())
        }
        Value::Integer(idx) => Some(c.methods[(idx & 0x00FF_FFFF) as usize].val.clone()),
        _ => None,
    }
}

/// `SQClass::CreateInstance` — locks the class, copies the defaults.
pub(super) fn create_instance(class: &ClassRef) -> InstanceRef {
    lock_class(class);
    let values = class
        .borrow()
        .default_values
        .iter()
        .map(|m| m.val.clone())
        .collect();
    Rc::new(RefCell::new(Instance {
        class: class.clone(),
        values,
        user: None,
    }))
}

fn lock_class(class: &ClassRef) {
    let base = {
        let mut c = class.borrow_mut();
        c.locked = true;
        c.base.clone()
    };
    if let Some(base) = base {
        lock_class(&base);
    }
}

/// `SQInstance::Get`.
fn instance_get(instance: &InstanceRef, key: &Value) -> Option<Value> {
    let i = instance.borrow();
    let c = i.class.borrow();
    match c.members.get(key)? {
        Value::Integer(idx) if idx & MEMBER_FIELD != 0 => {
            Some(i.values[(idx & 0x00FF_FFFF) as usize].real())
        }
        Value::Integer(idx) => Some(c.methods[(idx & 0x00FF_FFFF) as usize].val.clone()),
        _ => None,
    }
}

/// `SQInstance::Set` — fields only.
fn instance_set(instance: &InstanceRef, key: &Value, val: &Value) -> bool {
    let mut i = instance.borrow_mut();
    let idx = i.class.borrow().members.get(key);
    match idx {
        Some(Value::Integer(idx)) if idx & MEMBER_FIELD != 0 => {
            i.values[(idx & 0x00FF_FFFF) as usize] = val.clone();
            true
        }
        _ => false,
    }
}

fn same_object(a: &Value, b: &Value) -> bool {
    a.type_bit() == b.type_bit() && a.address() == b.address() && a.address() != 0
}

/// `PrintObjVal` — how an index or a compared value is named in an error.
fn print_obj_val(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => format_g(*f as f64, 14, false),
        other => other.type_name().to_owned(),
    }
}

/// `Raise_IdxError`, including the `%.50s`.
fn idx_error(key: &Value) -> Value {
    let name: String = print_obj_val(key).chars().take(50).collect();
    err(format!("the index '{name}' does not exist"))
}

impl Vm {
    // ----- the object model ----------------------------------------------

    fn this_value(&self) -> Value {
        self.frames
            .last()
            .and_then(|f| f.locals.first().cloned())
            .unwrap_or_default()
    }

    /// `GetMetaMethod` for a delegable object: a table's comes from its
    /// delegate's own slots, an instance's from its class.
    pub(super) fn metamethod(&self, object: &Value, mm: MetaMethod) -> Option<Value> {
        match object {
            Value::Table(t) => {
                let delegate = t.borrow().delegate.clone()?;
                let name = Value::str(super::METAMETHODS[mm as usize]);
                let found = delegate.borrow().table.get(&name);
                found
            }
            Value::Instance(i) => {
                let class = i.borrow().class.clone();
                let v = class.borrow().metamethods[mm as usize].clone();
                (!v.is_null()).then_some(v)
            }
            _ => None,
        }
    }

    /// `CallMetaMethod` — `None` if there is none or the call failed, which
    /// Squirrel does not distinguish.
    pub(super) fn call_metamethod(
        &mut self,
        host: &mut dyn Any,
        object: &Value,
        mm: MetaMethod,
        args: &[Value],
    ) -> Option<Value> {
        let function = self.metamethod(object, mm)?;
        match self.call_from_native(host, &function, object.clone(), args, false) {
            Ok(v) => Some(v),
            Err(e) => {
                self.last_error = e;
                None
            }
        }
    }

    /// `SQVM::Get`.
    pub(crate) fn get_value(
        &mut self,
        host: &mut dyn Any,
        object: &Value,
        key: &Value,
        raw: bool,
        fetch_root: bool,
    ) -> Option<Value> {
        match object {
            Value::Table(t) => {
                if let Some(v) = t.borrow().table.get(key) {
                    return Some(v);
                }
            }
            Value::Array(a) => {
                if key.is_numeric() {
                    let i = key.to_integer();
                    let a = a.borrow();
                    return (i >= 0 && (i as usize) < a.len()).then(|| a[i as usize].real());
                }
            }
            Value::Instance(i) => {
                if let Some(v) = instance_get(i, key) {
                    return Some(v);
                }
            }
            _ => {}
        }
        if let Some(v) = self.fallback_get(host, object, key, raw) {
            return Some(v);
        }
        if fetch_root && same_object(&self.this_value(), object) {
            return self.root.borrow().table.get(key);
        }
        None
    }

    /// `SQVM::FallBackGet`.
    fn fallback_get(&mut self, host: &mut dyn Any, object: &Value, key: &Value, raw: bool) -> Option<Value> {
        match object {
            Value::Class(c) => class_get(c, key),
            Value::Table(t) => {
                let delegate = t.borrow().delegate.clone();
                if let Some(delegate) = delegate {
                    if let Some(v) = self.get_value(host, &Value::Table(delegate), key, raw, false) {
                        return Some(v);
                    }
                    if raw {
                        return None;
                    }
                    if let Some(v) = self.call_metamethod(host, object, MetaMethod::Get, &[key.clone()]) {
                        return Some(v);
                    }
                }
                if raw {
                    return None;
                }
                self.delegates.table.borrow().table.get(key)
            }
            Value::Array(_) => {
                if raw {
                    return None;
                }
                self.delegates.array.borrow().table.get(key)
            }
            Value::String(s) => {
                if key.is_numeric() {
                    let n = key.to_integer();
                    let len = s.len() as i32;
                    if n.unsigned_abs() < len as u32 {
                        // `if(n<0)n=_string(self)->_len-n;` reads past the end
                        // of the string for a negative index — undefined
                        // behaviour, which the NUL after the buffer usually
                        // turns into 0.
                        let index = if n < 0 { len - n } else { n } as usize;
                        let byte = s.as_bytes().get(index).copied().unwrap_or(0);
                        return Some(Value::Integer(byte as i8 as i32));
                    }
                    return None;
                }
                if raw {
                    return None;
                }
                self.delegates.string.borrow().table.get(key)
            }
            Value::Instance(_) => {
                if raw {
                    return None;
                }
                if let Some(v) = self.call_metamethod(host, object, MetaMethod::Get, &[key.clone()]) {
                    return Some(v);
                }
                self.delegates.instance.borrow().table.get(key)
            }
            Value::Integer(_) | Value::Float(_) | Value::Bool(_) => {
                if raw {
                    return None;
                }
                self.delegates.number.borrow().table.get(key)
            }
            Value::Closure(_) | Value::Native(_) => {
                if raw {
                    return None;
                }
                self.delegates.closure.borrow().table.get(key)
            }
            Value::WeakRef(_) => {
                if raw {
                    return None;
                }
                self.delegates.weakref.borrow().table.get(key)
            }
            Value::Null => None,
        }
    }

    /// `SQVM::Set` — replaces an existing slot and never makes one. Errors
    /// for arrays and non-containers are raised here; a plain "not found"
    /// comes back as `false` for the caller to report.
    pub(crate) fn set_value(
        &mut self,
        host: &mut dyn Any,
        object: &Value,
        key: &Value,
        val: &Value,
        fetch_root: bool,
    ) -> bool {
        match object {
            Value::Table(t) => {
                if t.borrow_mut().table.set(key, val.clone()) {
                    return true;
                }
                let delegate = t.borrow().delegate.clone();
                if let Some(delegate) = delegate {
                    if self.set_value(host, &Value::Table(delegate), key, val, false) {
                        return true;
                    }
                    if self
                        .call_metamethod(host, object, MetaMethod::Set, &[key.clone(), val.clone()])
                        .is_some()
                    {
                        return true;
                    }
                }
            }
            Value::Instance(i) => {
                if instance_set(i, key, val) {
                    return true;
                }
                if self
                    .call_metamethod(host, object, MetaMethod::Set, &[key.clone(), val.clone()])
                    .is_some()
                {
                    return true;
                }
            }
            Value::Array(a) => {
                if !key.is_numeric() {
                    self.last_error = err(format!(
                        "indexing {} with {}",
                        object.type_name(),
                        key.type_name()
                    ));
                    return false;
                }
                let i = key.to_integer();
                let mut a = a.borrow_mut();
                if i >= 0 && (i as usize) < a.len() {
                    a[i as usize] = val.clone();
                    return true;
                }
                return false;
            }
            other => {
                self.last_error = err(format!("trying to set '{}'", other.type_name()));
                return false;
            }
        }
        if fetch_root && same_object(&self.this_value(), object) {
            return self.root.borrow_mut().table.set(key, val.clone());
        }
        false
    }

    /// `SQVM::NewSlot`.
    pub(crate) fn new_slot(
        &mut self,
        host: &mut dyn Any,
        object: &Value,
        key: Value,
        val: Value,
        is_static: bool,
    ) -> R<()> {
        if key.is_null() {
            return Err(err("null cannot be used as index"));
        }
        match object {
            Value::Table(t) => {
                let mut raw_call = true;
                let has_delegate = t.borrow().delegate.is_some();
                if has_delegate && !t.borrow().table.contains(&key) {
                    raw_call = self
                        .call_metamethod(host, object, MetaMethod::NewSlot, &[key.clone(), val.clone()])
                        .is_none();
                }
                if raw_call {
                    t.borrow_mut().table.new_slot(key, val);
                }
                Ok(())
            }
            Value::Instance(_) => {
                match self.call_metamethod(host, object, MetaMethod::NewSlot, &[key, val]) {
                    Some(_) => Ok(()),
                    None => Err(err("class instances do not support the new slot operator")),
                }
            }
            Value::Class(c) => {
                if class_new_slot(c, key, val, is_static) {
                    Ok(())
                } else {
                    Err(err("trying to modify a class that has already been instantiated"))
                }
            }
            other => Err(err(format!(
                "indexing {} with {}",
                other.type_name(),
                key.type_name()
            ))),
        }
    }

    /// `SQVM::DeleteSlot`.
    pub(crate) fn delete_slot(&mut self, host: &mut dyn Any, object: &Value, key: &Value) -> R<Value> {
        match object {
            Value::Table(_) | Value::Instance(_) => {
                if let Some(v) = self.call_metamethod(host, object, MetaMethod::DelSlot, &[key.clone()]) {
                    return Ok(v);
                }
                match object {
                    Value::Table(t) => {
                        let old = t.borrow().table.get(key);
                        match old {
                            Some(old) => {
                                t.borrow_mut().table.remove(key);
                                Ok(old)
                            }
                            None => Err(idx_error(key)),
                        }
                    }
                    other => Err(err(format!("cannot delete a slot from {}", other.type_name()))),
                }
            }
            other => Err(err(format!("attempt to delete a slot from a {}", other.type_name()))),
        }
    }

    /// `SQVM::Clone`.
    fn clone_value(&mut self, host: &mut dyn Any, v: &Value) -> Option<Value> {
        match v {
            Value::Table(t) => {
                let copy = {
                    let t = t.borrow();
                    TableObj {
                        table: t.table.clone_table(),
                        delegate: t.delegate.clone(),
                    }
                };
                let copy = Rc::new(RefCell::new(copy));
                self.track_table(&copy);
                let new = Value::Table(copy);
                self.call_metamethod(host, &new, MetaMethod::Cloned, &[v.clone()]);
                Some(new)
            }
            Value::Instance(i) => {
                let copy = {
                    let i = i.borrow();
                    Instance {
                        class: i.class.clone(),
                        values: i.values.clone(),
                        user: None,
                    }
                };
                let copy = Rc::new(RefCell::new(copy));
                self.track(Rc::downgrade(&copy) as Weak<dyn Finalize>);
                let new = Value::Instance(copy);
                self.call_metamethod(host, &new, MetaMethod::Cloned, &[v.clone()]);
                Some(new)
            }
            Value::Array(a) => {
                let copy = a.borrow().clone();
                Some(Value::Array(self.new_array(copy)))
            }
            _ => None,
        }
    }

    // ----- arithmetic and comparison -------------------------------------

    /// `SQVM::ToString`.
    pub(crate) fn value_to_string(&mut self, host: &mut dyn Any, v: &Value) -> SqStr {
        match v {
            Value::String(s) => s.clone(),
            Value::Float(f) => SqStr::from_str(&format_g(*f as f64, 6, false)),
            Value::Integer(i) => SqStr::from_str(&i.to_string()),
            Value::Bool(b) => SqStr::from_str(if *b { "true" } else { "false" }),
            Value::WeakRef(w) => {
                let inner = self.value_to_string(host, &w.get());
                SqStr::from_str(&format!(
                    "(weakref : 0x{:08X} [{}] )",
                    v.address() as u32,
                    inner.to_string_lossy()
                ))
            }
            Value::Table(_) | Value::Instance(_) => {
                if let Some(Value::String(s)) = self.call_metamethod(host, v, MetaMethod::ToString, &[]) {
                    return s;
                }
                SqStr::from_str(&v.to_display_string())
            }
            other => SqStr::from_str(&other.to_display_string()),
        }
    }

    /// `SQVM::ARITH_OP`.
    pub(crate) fn arith(&mut self, host: &mut dyn Any, op: ArithOp, a: &Value, b: &Value) -> R<Value> {
        if a.is_numeric() && b.is_numeric() {
            if let (Value::Integer(x), Value::Integer(y)) = (a, b) {
                let (x, y) = (*x, *y);
                let r = match op {
                    ArithOp::Add => x.wrapping_add(y),
                    ArithOp::Sub => x.wrapping_sub(y),
                    ArithOp::Mul => x.wrapping_mul(y),
                    ArithOp::Div => {
                        if y == 0 {
                            return Err(err("division by zero"));
                        }
                        x.wrapping_div(y)
                    }
                    ArithOp::Mod => {
                        if y == 0 {
                            return Err(err("modulo by zero"));
                        }
                        x.wrapping_rem(y)
                    }
                };
                return Ok(Value::Integer(r));
            }
            let (x, y) = (a.to_float(), b.to_float());
            let r = match op {
                ArithOp::Add => x + y,
                ArithOp::Sub => x - y,
                ArithOp::Mul => x * y,
                ArithOp::Div => x / y,
                ArithOp::Mod => ((x as f64) % (y as f64)) as f32,
            };
            return Ok(Value::Float(r));
        }
        if op == ArithOp::Add && (matches!(a, Value::String(_)) || matches!(b, Value::String(_))) {
            let x = self.value_to_string(host, a);
            let y = self.value_to_string(host, b);
            let mut bytes = Vec::with_capacity(x.len() + y.len());
            bytes.extend_from_slice(x.as_bytes());
            bytes.extend_from_slice(y.as_bytes());
            return Ok(Value::bytes(&bytes));
        }
        let mm = match op {
            ArithOp::Add => MetaMethod::Add,
            ArithOp::Sub => MetaMethod::Sub,
            ArithOp::Mul => MetaMethod::Mul,
            ArithOp::Div => MetaMethod::Div,
            ArithOp::Mod => MetaMethod::Modulo,
        };
        if matches!(a, Value::Table(_) | Value::Instance(_)) {
            if let Some(v) = self.call_metamethod(host, a, mm, &[b.clone()]) {
                return Ok(v);
            }
        }
        // Valve's change: whatever `_lasterror` already holds is appended.
        let base = format!(
            "arith op {} on between '{}' and '{}'",
            op.symbol(),
            a.type_name(),
            b.type_name()
        );
        Err(match &self.last_error {
            Value::String(previous) => err(format!("{base} ({})", previous.to_string_lossy())),
            _ => err(base),
        })
    }

    /// `SQVM::BW_OP`.
    fn bitwise(op: BitOp, a: &Value, b: &Value) -> R<Value> {
        match (a, b) {
            (Value::Integer(x), Value::Integer(y)) => {
                let (x, y) = (*x, *y);
                Ok(Value::Integer(match op {
                    BitOp::And => x & y,
                    BitOp::Or => x | y,
                    BitOp::Xor => x ^ y,
                    BitOp::ShiftL => x.wrapping_shl(y as u32),
                    BitOp::ShiftR => x.wrapping_shr(y as u32),
                    BitOp::UShiftR => (x as u32).wrapping_shr(y as u32) as i32,
                }))
            }
            _ => Err(err(format!(
                "bitwise op between '{}' and '{}'",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    /// `SQVM::ObjCmp`.
    pub(crate) fn compare(&mut self, host: &mut dyn Any, a: &Value, b: &Value) -> R<i32> {
        let cmp_error = || {
            err(format!(
                "comparsion between '{}' and '{}'",
                print_obj_val(a).chars().take(50).collect::<String>(),
                print_obj_val(b).chars().take(50).collect::<String>()
            ))
        };
        if a.type_bit() == b.type_bit() {
            if raw_equal(a, b) {
                return Ok(0);
            }
            return match (a, b) {
                (Value::String(x), Value::String(y)) => Ok(match x.as_bytes().cmp(y.as_bytes()) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }),
                (Value::Integer(x), Value::Integer(y)) => Ok(x.wrapping_sub(*y)),
                (Value::Float(x), Value::Float(y)) => Ok(if x < y { -1 } else { 1 }),
                (Value::Table(_), _) | (Value::Instance(_), _) => {
                    if self.metamethod(a, MetaMethod::Cmp).is_some() {
                        return match self.call_metamethod(host, a, MetaMethod::Cmp, &[b.clone()]) {
                            Some(Value::Integer(r)) => Ok(r),
                            _ => Err(cmp_error()),
                        };
                    }
                    Ok(if a.address() < b.address() { -1 } else { 1 })
                }
                _ => Ok(if a.address() < b.address() { -1 } else { 1 }),
            };
        }
        if a.is_numeric() && b.is_numeric() {
            let (x, y) = match (a, b) {
                (Value::Integer(i), Value::Float(f)) => (*i as f32, *f),
                (Value::Float(f), Value::Integer(i)) => (*f, *i as f32),
                _ => unreachable!("two numbers of different types"),
            };
            return Ok(if x == y {
                0
            } else if x < y {
                -1
            } else {
                1
            });
        }
        if a.is_null() {
            return Ok(-1);
        }
        if b.is_null() {
            return Ok(1);
        }
        Err(cmp_error())
    }

    /// `SQVM::IsEqual`.
    pub(crate) fn is_equal(&mut self, host: &mut dyn Any, a: &Value, b: &Value) -> R<bool> {
        if a.type_bit() == b.type_bit() {
            return Ok(raw_equal(a, b));
        }
        if a.is_numeric() && b.is_numeric() {
            return Ok(self.compare(host, a, b)? == 0);
        }
        Ok(false)
    }

    /// `SQVM::TypeOf`.
    fn type_of(&mut self, host: &mut dyn Any, v: &Value) -> Value {
        if matches!(v, Value::Table(_) | Value::Instance(_)) {
            if let Some(t) = self.call_metamethod(host, v, MetaMethod::Typeof, &[]) {
                return t;
            }
        }
        Value::str(v.type_name())
    }

    // ----- errors ---------------------------------------------------------

    /// `Raise_Error` + `SQ_THROW`'s decision about the error handler.
    pub(crate) fn raise(&mut self, host: &mut dyn Any, error: Value) -> Value {
        self.last_error = error.clone();
        let report = self
            .executes
            .last()
            .is_some_and(|e| e.traps == 0 && e.raise_error);
        if report {
            self.call_error_handler(host, &error);
        }
        error
    }

    /// `SQVM::CallErrorHandler`.
    fn call_error_handler(&mut self, host: &mut dyn Any, error: &Value) {
        let handler = self.error_handler.clone();
        if handler.is_null() {
            return;
        }
        let root = Value::Table(self.root.clone());
        // The handler's own errors go nowhere, as in `Call( ..., SQFalse )`.
        let _ = self.call_from_native(host, &handler, root, std::slice::from_ref(error), false);
    }

    /// The callstack, from `level` up, as `sq_stackinfos` reports it:
    /// function name, source, line.
    pub(crate) fn stack_infos(&self, level: usize) -> Option<(String, String, i32)> {
        let index = self.frames.len().checked_sub(level + 1)?;
        let frame = &self.frames[index];
        Some(match (&frame.closure, &frame.native_name) {
            (Some(c), _) => (
                match &c.proto.name {
                    Value::String(s) => s.to_string_lossy(),
                    _ => "unknown".into(),
                },
                c.proto.source.to_string_lossy(),
                frame.line as i32,
            ),
            (None, Some(name)) => (name.to_string_lossy(), "NATIVE".into(), -1),
            (None, None) => ("unknown".into(), "NATIVE".into(), -1),
        })
    }

    /// `sq_getlocal` for every local of one level: the outers first, then the
    /// locals in scope.
    pub(crate) fn stack_locals(&self, level: usize) -> Vec<(String, Value)> {
        let Some(index) = self.frames.len().checked_sub(level + 1) else {
            return Vec::new();
        };
        let frame = &self.frames[index];
        let Some(closure) = &frame.closure else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (outer, value) in closure.proto.outers.iter().zip(&closure.outers) {
            let name = match outer {
                Outer::Symbol(s) => s.to_string_lossy(),
                _ => "outer".into(),
            };
            out.push((name, value.clone()));
        }
        for (name, slot) in &frame.live {
            out.push((
                name.to_string_lossy(),
                frame.locals.get(*slot as usize).cloned().unwrap_or_default(),
            ));
        }
        out
    }

    // ----- calls ----------------------------------------------------------

    /// `SQVM::Call` from native code: a closure gets a new `Execute`.
    pub(crate) fn call_from_native(
        &mut self,
        host: &mut dyn Any,
        function: &Value,
        this: Value,
        args: &[Value],
        raise_error: bool,
    ) -> R<Value> {
        match function {
            Value::Closure(c) => {
                if self.native_calls + 1 > MAX_NATIVE_CALLS {
                    return Err(err("Native stack overflow"));
                }
                self.native_calls += 1;
                self.executes.push(Execute {
                    traps: 0,
                    raise_error,
                });
                let result = self.call_closure(host, c, this, args);
                self.executes.pop();
                self.native_calls -= 1;
                match result {
                    Ok(v) => Ok(v),
                    Err(Raised(e)) => Err(e),
                    // `StartCall` failing is raised by nobody inside, and
                    // `Execute` reports it itself only when there is no
                    // script frame at all to report it later.
                    Err(StartCall(e)) => {
                        self.last_error = e.clone();
                        if self.frames.is_empty() && raise_error {
                            self.call_error_handler(host, &e);
                        }
                        Err(e)
                    }
                }
            }
            Value::Native(n) => self.call_native(host, n, this, args),
            Value::Class(class) => {
                let instance = self.new_instance(class, None);
                let instance = Value::Instance(instance);
                if let Some(constructor) = class_get(class, &Value::str("constructor")) {
                    if !constructor.is_null() {
                        self.call_from_native(host, &constructor, instance.clone(), args, raise_error)?;
                    }
                }
                Ok(instance)
            }
            other => Err(err(format!("attempt to call '{}'", other.type_name()))),
        }
    }

    /// A call made *by a script* — `_OP_CALL`. A closure runs in the current
    /// execution; a native's error is raised here, at the call site.
    fn call_in_script(&mut self, host: &mut dyn Any, function: &Value, this: Value, args: &[Value]) -> R<Value> {
        match function {
            Value::Closure(c) => match self.call_closure(host, c, this, args) {
                Ok(v) => Ok(v),
                // `StartCall` refusing the arguments is raised by the caller's
                // `_GUARD`; everything else was raised inside.
                Err(StartCall(e)) => Err(self.raise(host, e)),
                Err(Raised(e)) => Err(e),
            },
            Value::Native(n) => match self.call_native(host, n, this, args) {
                Ok(v) => Ok(v),
                Err(e) => Err(self.raise(host, e)),
            },
            Value::Class(class) => {
                let instance = Value::Instance(self.new_instance(class, None));
                if let Some(constructor) = class_get(class, &Value::str("constructor")) {
                    if !constructor.is_null() {
                        self.call_in_script(host, &constructor, instance.clone(), args)?;
                    }
                }
                Ok(instance)
            }
            Value::Table(_) | Value::Instance(_) => {
                let mut mm_args = Vec::with_capacity(args.len() + 1);
                mm_args.push(this);
                mm_args.extend_from_slice(args);
                match self.call_metamethod(host, function, MetaMethod::Call, &mm_args) {
                    Some(v) => Ok(v),
                    None => Err(self.raise(
                        host,
                        err(format!("attempt to call '{}'", function.type_name())),
                    )),
                }
            }
            other => Err(self.raise(host, err(format!("attempt to call '{}'", other.type_name())))),
        }
    }

    /// `SQVM::CallNative`.
    pub(crate) fn call_native(&mut self, host: &mut dyn Any, native: &Rc<Native>, this: Value, args: &[Value]) -> R<Value> {
        if self.native_calls + 1 > MAX_NATIVE_CALLS {
            return Err(err("Native stack overflow"));
        }
        let nargs = args.len() as i32 + 1;
        let check = native.nparamscheck;
        if (check > 0 && check != nargs) || (check < 0 && nargs < -check) {
            return Err(err("wrong number of parameters"));
        }
        let this = match &native.env {
            Some(env) => env.get(),
            None => this,
        };
        let mut full = Vec::with_capacity(args.len() + 1);
        full.push(this);
        full.extend_from_slice(args);
        for (i, (mask, value)) in native.typecheck.iter().zip(&full).enumerate() {
            if *mask != u32::MAX && value.type_bit() & mask == 0 {
                let mut expected = Vec::new();
                for bit in 0..16 {
                    let m = 1u32 << bit;
                    if mask & m != 0 {
                        expected.push(type_name_of_bit(m));
                    }
                }
                return Err(err(format!(
                    "parameter {} has an invalid type '{}' ; expected: '{}'",
                    i,
                    value.type_name(),
                    expected.join("|")
                )));
            }
        }
        self.native_calls += 1;
        self.frames.push(Frame {
            closure: None,
            native_name: Some(native.name.clone()),
            locals: Vec::new(),
            vargs: Vec::new(),
            line: 0,
            live: Vec::new(),
        });
        let func = native.func.clone();
        let result = func(self, host, &full);
        self.frames.pop();
        self.native_calls -= 1;
        match result {
            // `return SQ_ERROR` without `sq_throwerror` — `CallNative` then
            // raises whatever `_lasterror` already held, so a native that
            // fails quietly (Valve's `Vector._get` for any key but x, y, z)
            // must not overwrite it.
            Err(Value::Null) => Err(self.last_error.clone()),
            other => other,
        }
    }

    /// `SQVM::StartCall` plus the function body.
    fn call_closure(&mut self, host: &mut dyn Any, closure: &Rc<Closure>, this: Value, args: &[Value]) -> Result<Value, StartCallError> {
        let proto = closure.proto.clone();
        let nparams = proto.params.len();
        let nargs = args.len() + 1;
        let mut locals = vec![Value::Null; proto.stack_size.max(nparams)];
        locals[0] = this;
        let mut vargs = Vec::new();
        if nparams != nargs {
            let ndef = proto.n_defaults;
            if ndef > 0 && nargs < nparams {
                let diff = nparams - nargs;
                if diff > ndef {
                    return Err(StartCall(err("wrong number of parameters")));
                }
                for (i, a) in args.iter().enumerate() {
                    locals[i + 1] = a.clone();
                }
                for (k, n) in (ndef - diff..ndef).enumerate() {
                    locals[nargs + k] = closure.defaults.get(n).cloned().unwrap_or_default();
                }
            } else if proto.varparams {
                if nargs < nparams {
                    return Err(StartCall(err("wrong number of parameters")));
                }
                for (i, a) in args.iter().enumerate() {
                    if i + 1 < nparams {
                        locals[i + 1] = a.clone();
                    } else {
                        vargs.push(a.clone());
                    }
                }
            } else {
                return Err(StartCall(err("wrong number of parameters")));
            }
        } else {
            for (i, a) in args.iter().enumerate() {
                locals[i + 1] = a.clone();
            }
        }
        if let Some(env) = &closure.env {
            locals[0] = env.get();
        }
        if proto.generator {
            return Err(StartCall(err(
                "generators are not supported by this VM (no shipped script uses yield)",
            )));
        }
        if self.frames.len() >= MAX_SCRIPT_DEPTH {
            return Err(StartCall(err("stack overflow")));
        }
        let live = proto
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.clone(), i as u16))
            .collect();
        self.frames.push(Frame {
            closure: Some(closure.clone()),
            native_name: None,
            locals,
            vargs,
            line: proto.line,
            live,
        });
        let result = self.exec(host, &proto.body);
        self.frames.pop();
        match result {
            Ok(Flow::Return(v)) => Ok(v),
            Ok(_) => Ok(Value::Null),
            Err(e) => Err(Raised(e)),
        }
    }

    // ----- statements -----------------------------------------------------

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a script frame is running")
    }

    fn set_local(&mut self, slot: u16, value: Value) {
        self.frame().locals[slot as usize] = value;
    }

    fn local(&self, slot: u16) -> Value {
        self.frames.last().expect("a script frame is running").locals[slot as usize].clone()
    }

    fn declare(&mut self, name: &SqStr, slot: u16) {
        let frame = self.frame();
        frame.live.retain(|(_, s)| *s != slot);
        frame.live.push((name.clone(), slot));
    }

    fn scope_end(&mut self, live_len: usize) {
        self.frame().live.truncate(live_len);
    }

    fn exec_block(&mut self, host: &mut dyn Any, stmts: &[Stmt]) -> R<Flow> {
        let live = self.frame().live.len();
        for s in stmts {
            match self.exec(host, s)? {
                Flow::Normal => {}
                other => {
                    self.scope_end(live);
                    return Ok(other);
                }
            }
        }
        self.scope_end(live);
        Ok(Flow::Normal)
    }

    pub(super) fn exec(&mut self, host: &mut dyn Any, stmt: &Stmt) -> R<Flow> {
        self.frame().line = stmt.line;
        match &stmt.kind {
            StmtKind::Empty => Ok(Flow::Normal),
            StmtKind::Expr(e) => {
                self.eval(host, e)?;
                Ok(Flow::Normal)
            }
            StmtKind::Local(decls) => {
                for (name, slot, init) in decls {
                    let v = match init {
                        Some(e) => self.eval(host, e)?,
                        None => Value::Null,
                    };
                    self.set_local(*slot, v);
                    self.declare(name, *slot);
                }
                Ok(Flow::Normal)
            }
            StmtKind::If(cond, then, els) => {
                let live = self.frame().live.len();
                let c = self.eval(host, cond)?;
                let flow = if !c.is_false() {
                    self.exec(host, then)
                } else if let Some(els) = els {
                    self.exec(host, els)
                } else {
                    Ok(Flow::Normal)
                };
                self.scope_end(live);
                flow
            }
            StmtKind::While(cond, body) => {
                let live = self.frame().live.len();
                loop {
                    let c = self.eval(host, cond)?;
                    if c.is_false() {
                        break;
                    }
                    match self.exec(host, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => {
                            self.scope_end(live);
                            return Ok(Flow::Return(v));
                        }
                        _ => {}
                    }
                    self.scope_end(live);
                    self.back_edge(host)?;
                }
                self.scope_end(live);
                Ok(Flow::Normal)
            }
            StmtKind::DoWhile(body, cond) => {
                let live = self.frame().live.len();
                loop {
                    match self.exec(host, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => {
                            self.scope_end(live);
                            return Ok(Flow::Return(v));
                        }
                        _ => {}
                    }
                    self.scope_end(live);
                    let c = self.eval(host, cond)?;
                    if c.is_false() {
                        break;
                    }
                    self.back_edge(host)?;
                }
                self.scope_end(live);
                Ok(Flow::Normal)
            }
            StmtKind::For { init, cond, step, body } => {
                let live = self.frame().live.len();
                if let Some(init) = init {
                    self.exec(host, init)?;
                }
                let loop_live = self.frame().live.len();
                loop {
                    if let Some(cond) = cond {
                        if self.eval(host, cond)?.is_false() {
                            break;
                        }
                    }
                    match self.exec(host, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => {
                            self.scope_end(live);
                            return Ok(Flow::Return(v));
                        }
                        _ => {}
                    }
                    self.scope_end(loop_live);
                    if let Some(step) = step {
                        self.eval(host, step)?;
                    }
                    self.back_edge(host)?;
                }
                self.scope_end(live);
                Ok(Flow::Normal)
            }
            StmtKind::Foreach { key, value, container, body } => {
                let live = self.frame().live.len();
                let container = self.eval(host, container)?;
                self.declare(&key.0, key.1);
                self.declare(&value.0, value.1);
                let loop_live = self.frame().live.len();
                self.set_local(key.1, Value::Null);
                self.set_local(value.1, Value::Null);
                let mut position = Value::Null;
                loop {
                    let Some((k, v, next)) = self.foreach_next(host, &container, &position)? else {
                        break;
                    };
                    position = next;
                    self.set_local(key.1, k);
                    self.set_local(value.1, v);
                    match self.exec(host, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => {
                            self.scope_end(live);
                            return Ok(Flow::Return(v));
                        }
                        _ => {}
                    }
                    self.scope_end(loop_live);
                    self.back_edge(host)?;
                }
                self.scope_end(live);
                Ok(Flow::Normal)
            }
            StmtKind::Switch { value, cases, default } => {
                let live = self.frame().live.len();
                let v = self.eval(host, value)?;
                let mut matched = false;
                let mut flow = Flow::Normal;
                'cases: {
                    for (test, stmts) in cases {
                        if !matched {
                            let t = self.eval(host, test)?;
                            matched = self.is_equal(host, &t, &v)?;
                        }
                        if matched {
                            match self.exec_block(host, stmts)? {
                                Flow::Normal => {}
                                Flow::Break => break 'cases,
                                other => {
                                    flow = other;
                                    break 'cases;
                                }
                            }
                        }
                    }
                    if let Some(stmts) = default {
                        match self.exec_block(host, stmts)? {
                            Flow::Normal | Flow::Break => {}
                            other => flow = other,
                        }
                    }
                }
                self.scope_end(live);
                Ok(flow)
            }
            StmtKind::Block(stmts) => self.exec_block(host, stmts),
            StmtKind::Return(value) => {
                let v = match value {
                    Some(e) => self.eval(host, e)?,
                    None => Value::Null,
                };
                self.tick_query().map_err(|e| self.raise(host, e))?;
                Ok(Flow::Return(v))
            }
            StmtKind::Yield => Err(self.raise(
                host,
                err("trying to yield a 'null',only genenerator can be yielded"),
            )),
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Try { body, catch, handler } => {
                let live = self.frame().live.len();
                let depth = self.frames.len();
                if let Some(e) = self.executes.last_mut() {
                    e.traps += 1;
                }
                let result = self.exec(host, body);
                if let Some(e) = self.executes.last_mut() {
                    e.traps -= 1;
                }
                match result {
                    Ok(flow) => {
                        self.scope_end(live);
                        Ok(flow)
                    }
                    Err(error) => {
                        // Unwind to this frame — anything the error passed
                        // through has already been popped by its caller.
                        debug_assert_eq!(self.frames.len(), depth);
                        self.scope_end(live);
                        self.set_local(catch.1, error);
                        self.declare(&catch.0, catch.1);
                        let flow = self.exec(host, handler);
                        self.scope_end(live);
                        flow
                    }
                }
            }
            StmtKind::Throw(e) => {
                let v = self.eval(host, e)?;
                Err(self.raise(host, v))
            }
        }
    }

    /// `_OP_JMP` backwards: where `SQVM` asks whether a script has run too
    /// long.
    fn back_edge(&mut self, host: &mut dyn Any) -> R<()> {
        self.tick_query().map_err(|e| self.raise(host, e))
    }

    /// `SQVM::FOREACH_OP` — the next key and value after `position`, and the
    /// position to resume from.
    fn foreach_next(&mut self, host: &mut dyn Any, container: &Value, position: &Value) -> R<Option<(Value, Value, Value)>> {
        let start = match position {
            Value::Integer(i) => *i as usize,
            _ => 0,
        };
        match container {
            Value::Table(t) => Ok(t
                .borrow()
                .table
                .next(start, false)
                .map(|(next, k, v)| (k, v, Value::Integer(next as i32)))),
            Value::Array(a) => {
                let a = a.borrow();
                Ok((start < a.len()).then(|| {
                    (Value::Integer(start as i32), a[start].real(), Value::Integer(start as i32 + 1))
                }))
            }
            Value::String(s) => Ok((start < s.len()).then(|| {
                (
                    Value::Integer(start as i32),
                    Value::Integer(s.as_bytes()[start] as i8 as i32),
                    Value::Integer(start as i32 + 1),
                )
            })),
            Value::Class(c) => {
                let c = c.borrow();
                Ok(c.members.next(start, false).map(|(next, k, idx)| {
                    let idx = idx.to_integer();
                    let v = if idx & MEMBER_FIELD != 0 {
                        c.default_values[(idx & 0x00FF_FFFF) as usize].val.real()
                    } else {
                        c.methods[(idx & 0x00FF_FFFF) as usize].val.clone()
                    };
                    (k, v, Value::Integer(next as i32))
                }))
            }
            Value::Instance(_) => {
                if self.metamethod(container, MetaMethod::Nexti).is_some() {
                    let itr = match self.call_metamethod(host, container, MetaMethod::Nexti, &[position.clone()]) {
                        Some(itr) => itr,
                        None => return Err(self.raise(host, err("_nexti failed"))),
                    };
                    if itr.is_null() {
                        return Ok(None);
                    }
                    match self.get_value(host, container, &itr, false, false) {
                        Some(v) => Ok(Some((itr.clone(), v, itr))),
                        None => Err(self.raise(host, err("_nexti returned an invalid idx"))),
                    }
                } else {
                    Err(self.raise(host, err(format!("cannot iterate {}", container.type_name()))))
                }
            }
            other => Err(self.raise(host, err(format!("cannot iterate {}", other.type_name())))),
        }
    }

    // ----- expressions ----------------------------------------------------

    fn eval_get(&mut self, host: &mut dyn Any, object: &Value, key: &Value) -> R<Value> {
        match self.get_value(host, object, key, false, true) {
            Some(v) => Ok(v),
            None => Err(self.raise(host, idx_error(key))),
        }
    }

    pub(super) fn eval(&mut self, host: &mut dyn Any, e: &Expr) -> R<Value> {
        match e {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(i) => Ok(Value::Integer(*i)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Str(s) => Ok(Value::String(s.clone())),
            Expr::Const(v) => Ok(v.clone()),
            Expr::Local(slot) => Ok(self.local(*slot)),
            Expr::Outer(i) => {
                let frame = self.frames.last().expect("a script frame is running");
                Ok(frame
                    .closure
                    .as_ref()
                    .and_then(|c| c.outers.get(*i as usize).cloned())
                    .unwrap_or_default())
            }
            Expr::Root => Ok(Value::Table(self.root.clone())),
            Expr::Get(obj, key) => {
                let o = self.eval(host, obj)?;
                let k = self.eval(host, key)?;
                self.eval_get(host, &o, &k)
            }
            Expr::Parent(obj) => {
                let o = self.eval(host, obj)?;
                match &o {
                    Value::Table(t) => Ok(t.borrow().delegate.clone().map_or(Value::Null, Value::Table)),
                    Value::Class(c) => Ok(c.borrow().base.clone().map_or(Value::Null, Value::Class)),
                    other => Err(self.raise(
                        host,
                        err(format!("the {} type doesn't have a parent slot", other.type_name())),
                    )),
                }
            }
            Expr::Vargc => Ok(Value::Integer(
                self.frames.last().map_or(0, |f| f.vargs.len() as i32),
            )),
            Expr::Vargv(index) => {
                let i = self.eval(host, index)?;
                let vargs_len = self.frames.last().map_or(0, |f| f.vargs.len());
                if vargs_len == 0 {
                    return Err(self.raise(host, err("the function doesn't have var args")));
                }
                if !i.is_numeric() {
                    return Err(self.raise(host, err(format!("indexing 'vargv' with {}", i.type_name()))));
                }
                let i = i.to_integer();
                if i < 0 || i as usize >= vargs_len {
                    return Err(self.raise(host, err("vargv index out of range")));
                }
                Ok(self.frames.last().expect("a frame").vargs[i as usize].clone())
            }
            Expr::Group(inner) => self.eval(host, inner),
            Expr::Call(callee, args) => self.eval_call(host, callee, args),
            Expr::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(host, item)?);
                }
                Ok(Value::Array(self.new_array(values)))
            }
            Expr::Table(slots) => {
                let table = self.new_table(slots.len());
                let t = Value::Table(table);
                for (key, value) in slots {
                    let k = self.eval(host, key)?;
                    let v = self.eval(host, value)?;
                    self.new_slot(host, &t, k, v, false).map_err(|e| self.raise(host, e))?;
                }
                Ok(t)
            }
            Expr::Function(proto) => self.make_closure(host, proto),
            Expr::Class(class) => self.eval_class(host, class),
            Expr::Unary(op, operand) => {
                let v = self.eval(host, operand)?;
                self.eval_unary(host, *op, v)
            }
            Expr::Binary(op, a, b) => {
                let x = self.eval(host, a)?;
                let y = self.eval(host, b)?;
                self.eval_binary(host, *op, &x, &y)
            }
            Expr::And(a, b) => {
                let x = self.eval(host, a)?;
                if x.is_false() {
                    return Ok(x);
                }
                self.eval(host, b)
            }
            Expr::Or(a, b) => {
                let x = self.eval(host, a)?;
                if !x.is_false() {
                    return Ok(x);
                }
                self.eval(host, b)
            }
            Expr::Ternary(c, a, b) => {
                let cond = self.eval(host, c)?;
                match cond.is_false() {
                    false => self.eval(host, a),
                    true => self.eval(host, b),
                }
            }
            Expr::Assign(target, value) => match &**target {
                Expr::Local(slot) => {
                    let v = self.eval(host, value)?;
                    self.set_local(*slot, v.clone());
                    Ok(v)
                }
                Expr::Get(obj, key) => {
                    let o = self.eval(host, obj)?;
                    let k = self.eval(host, key)?;
                    let v = self.eval(host, value)?;
                    if !self.set_value(host, &o, &k, &v, true) {
                        return Err(self.raise(host, idx_error(&k)));
                    }
                    Ok(v)
                }
                _ => Err(self.raise(host, err("can't assign expression"))),
            },
            Expr::NewSlot(target, value) => {
                let Expr::Get(obj, key) = &**target else {
                    return Err(self.raise(host, err("can't 'create' a local slot")));
                };
                let o = self.eval(host, obj)?;
                let k = self.eval(host, key)?;
                let v = self.eval(host, value)?;
                self.new_slot(host, &o, k, v.clone(), false)
                    .map_err(|e| self.raise(host, e))?;
                Ok(v)
            }
            Expr::Compound(op, target, value) => match &**target {
                Expr::Local(slot) => {
                    let v = self.eval(host, value)?;
                    let current = self.local(*slot);
                    let r = self.arith(host, *op, &current, &v).map_err(|e| self.raise(host, e))?;
                    self.set_local(*slot, r.clone());
                    Ok(r)
                }
                Expr::Get(obj, key) => {
                    let o = self.eval(host, obj)?;
                    let k = self.eval(host, key)?;
                    let v = self.eval(host, value)?;
                    self.deref_incr(host, *op, &o, &k, &v, false)
                }
                other => {
                    let current = self.eval(host, other)?;
                    let v = self.eval(host, value)?;
                    self.arith(host, *op, &current, &v).map_err(|e| self.raise(host, e))
                }
            },
            Expr::PreIncr(target, delta) => self.incr(host, target, *delta, false),
            Expr::PostIncr(target, delta) => self.incr(host, target, *delta, true),
            Expr::Delete(target) => {
                let Expr::Get(obj, key) = &**target else {
                    return Err(self.raise(host, err("can't delete an expression")));
                };
                let o = self.eval(host, obj)?;
                let k = self.eval(host, key)?;
                self.delete_slot(host, &o, &k).map_err(|e| self.raise(host, e))
            }
            Expr::Delegate(delegate, table) => {
                let d = self.eval(host, delegate)?;
                let t = self.eval(host, table)?;
                let Value::Table(target) = &t else {
                    return Err(self.raise(host, err(format!("delegating a '{}'", t.type_name()))));
                };
                match &d {
                    Value::Table(new) => {
                        // `SetDelegate`'s cycle check.
                        if Rc::ptr_eq(new, target) {
                            return Err(self.raise(host, err("delegate cycle detected")));
                        }
                        let mut walk = Some(new.clone());
                        while let Some(w) = walk {
                            let next = w.borrow().delegate.clone();
                            if next.as_ref().is_some_and(|n| Rc::ptr_eq(n, target)) {
                                return Err(self.raise(host, err("delegate cycle detected")));
                            }
                            walk = next;
                        }
                        target.borrow_mut().delegate = Some(new.clone());
                    }
                    Value::Null => target.borrow_mut().delegate = None,
                    other => {
                        return Err(self.raise(host, err(format!("using '{}' as delegate", other.type_name()))))
                    }
                }
                Ok(t)
            }
            Expr::Comma(all) => {
                let mut last = Value::Null;
                for e in all {
                    last = self.eval(host, e)?;
                }
                Ok(last)
            }
        }
    }

    /// `_OP_INC`/`_OP_INCL`/`_OP_PINC`/`_OP_PINCL`.
    fn incr(&mut self, host: &mut dyn Any, target: &Expr, delta: i32, postfix: bool) -> R<Value> {
        let d = Value::Integer(delta);
        match target {
            Expr::Local(slot) => {
                let old = self.local(*slot);
                let new = self.arith(host, ArithOp::Add, &old, &d).map_err(|e| self.raise(host, e))?;
                self.set_local(*slot, new.clone());
                Ok(if postfix { old } else { new })
            }
            Expr::Get(obj, key) => {
                let o = self.eval(host, obj)?;
                let k = self.eval(host, key)?;
                self.deref_incr(host, ArithOp::Add, &o, &k, &d, postfix)
            }
            other => {
                let old = self.eval(host, other)?;
                let new = self.arith(host, ArithOp::Add, &old, &d).map_err(|e| self.raise(host, e))?;
                Ok(if postfix { old } else { new })
            }
        }
    }

    /// `SQVM::DerefInc` — the `Set` at the end is not checked, as in the C.
    fn deref_incr(&mut self, host: &mut dyn Any, op: ArithOp, o: &Value, k: &Value, v: &Value, postfix: bool) -> R<Value> {
        let current = match self.get_value(host, o, k, false, true) {
            Some(c) => c,
            None => return Err(self.raise(host, idx_error(k))),
        };
        let new = self.arith(host, op, &current, v).map_err(|e| self.raise(host, e))?;
        self.set_value(host, o, k, &new, true);
        Ok(if postfix { current } else { new })
    }

    fn eval_unary(&mut self, host: &mut dyn Any, op: UnaryOp, v: Value) -> R<Value> {
        match op {
            UnaryOp::Neg => match &v {
                Value::Integer(i) => Ok(Value::Integer(i.wrapping_neg())),
                Value::Float(f) => Ok(Value::Float(-f)),
                Value::Table(_) | Value::Instance(_) => {
                    match self.call_metamethod(host, &v, MetaMethod::Unm, &[]) {
                        Some(r) => Ok(r),
                        None => Err(self.raise(host, err(format!("attempt to negate a {}", v.type_name())))),
                    }
                }
                other => Err(self.raise(host, err(format!("attempt to negate a {}", other.type_name())))),
            },
            UnaryOp::Not => Ok(Value::Bool(v.is_false())),
            UnaryOp::BitNot => match v {
                Value::Integer(i) => Ok(Value::Integer(!i)),
                other => Err(self.raise(
                    host,
                    err(format!("attempt to perform a bitwise op on a {}", other.type_name())),
                )),
            },
            UnaryOp::Typeof => Ok(self.type_of(host, &v)),
            UnaryOp::Clone => match self.clone_value(host, &v) {
                Some(c) => Ok(c),
                None => Err(self.raise(host, err(format!("cloning a {}", v.type_name())))),
            },
            UnaryOp::Resume => Err(self.raise(
                host,
                err(format!(
                    "trying to resume a '{}',only genenerator can be resumed",
                    v.type_name()
                )),
            )),
        }
    }

    fn eval_binary(&mut self, host: &mut dyn Any, op: BinOp, a: &Value, b: &Value) -> R<Value> {
        let result = match op {
            BinOp::Arith(op) => self.arith(host, op, a, b),
            BinOp::Bit(op) => Self::bitwise(op, a, b),
            BinOp::Cmp(op) => self.compare(host, a, b).map(|r| {
                Value::Bool(match op {
                    CmpOp::Greater => r > 0,
                    CmpOp::GreaterEq => r >= 0,
                    CmpOp::Less => r < 0,
                    CmpOp::LessEq => r <= 0,
                })
            }),
            BinOp::Eq => self.is_equal(host, a, b).map(Value::Bool),
            BinOp::Ne => self.is_equal(host, a, b).map(|r| Value::Bool(!r)),
            BinOp::In => Ok(Value::Bool(self.get_value(host, b, a, true, false).is_some())),
            BinOp::InstanceOf => match (a, b) {
                (Value::Instance(i), Value::Class(c)) => {
                    let mut walk = Some(i.borrow().class.clone());
                    let mut found = false;
                    while let Some(k) = walk {
                        if Rc::ptr_eq(&k, c) {
                            found = true;
                            break;
                        }
                        walk = k.borrow().base.clone();
                    }
                    Ok(Value::Bool(found))
                }
                _ => Err(err(format!(
                    "cannot apply instanceof between a {} and a {}",
                    b.type_name(),
                    a.type_name()
                ))),
            },
        };
        result.map_err(|e| self.raise(host, e))
    }

    /// `_OP_PREPCALL` + `_OP_CALL`: `obj.f(...)` binds `this` to `obj` (unless
    /// `obj` is a class, when it keeps the caller's); anything else gets the
    /// caller's `this`.
    fn eval_call(&mut self, host: &mut dyn Any, callee: &Expr, args: &[Expr]) -> R<Value> {
        let (function, this) = match callee {
            Expr::Get(obj, key) => {
                let o = self.eval(host, obj)?;
                let k = self.eval(host, key)?;
                let f = match self.get_value(host, &o, &k, false, true) {
                    Some(f) => f,
                    None => {
                        let from_class = match &o {
                            Value::Class(_) => self.delegates.class.borrow().table.get(&k),
                            _ => None,
                        };
                        match from_class {
                            Some(f) => f,
                            None => return Err(self.raise(host, idx_error(&k))),
                        }
                    }
                };
                let this = match o {
                    Value::Class(_) => self.this_value(),
                    other => other,
                };
                (f, this)
            }
            other => {
                let f = self.eval(host, other)?;
                (f, self.this_value())
            }
        };
        let mut values = Vec::with_capacity(args.len());
        for a in args {
            values.push(self.eval(host, a)?);
        }
        self.call_in_script(host, &function, this, &values)
    }

    /// `SQVM::CLOSURE_OP`.
    fn make_closure(&mut self, host: &mut dyn Any, proto: &Rc<FuncProto>) -> R<Value> {
        let mut outers = Vec::with_capacity(proto.outers.len());
        for outer in &proto.outers {
            let v = match outer {
                Outer::Local(slot) => self.local(*slot),
                Outer::Outer(i) => self
                    .frames
                    .last()
                    .and_then(|f| f.closure.as_ref())
                    .and_then(|c| c.outers.get(*i as usize).cloned())
                    .unwrap_or_default(),
                Outer::Symbol(name) => {
                    let this = self.this_value();
                    let key = Value::String(name.clone());
                    match self.get_value(host, &this, &key, false, true) {
                        Some(v) => v,
                        None => return Err(self.raise(host, idx_error(&key))),
                    }
                }
            };
            outers.push(v);
        }
        let mut defaults = Vec::with_capacity(proto.default_exprs.len());
        for d in &proto.default_exprs {
            defaults.push(self.eval(host, d)?);
        }
        Ok(Value::Closure(Rc::new(Closure {
            proto: proto.clone(),
            outers,
            defaults,
            env: None,
        })))
    }

    /// `SQVM::CLASS_OP` and the `_OP_NEWSLOTA`s that fill it.
    fn eval_class(&mut self, host: &mut dyn Any, class: &ClassExpr) -> R<Value> {
        let base = match &class.base {
            Some(b) => match self.eval(host, b)? {
                Value::Class(c) => Some(c),
                other => {
                    return Err(self.raise(
                        host,
                        err(format!("trying to inherit from a {}", other.type_name())),
                    ))
                }
            },
            None => None,
        };
        let attributes = match &class.attributes {
            Some(a) => self.eval(host, a)?,
            None => Value::Null,
        };
        let new = self.new_class(base, 0);
        let value = Value::Class(new.clone());
        let inherited = new.borrow().metamethods[MetaMethod::Inherited as usize].clone();
        if !inherited.is_null() {
            let _ = self.call_from_native(host, &inherited, value.clone(), &[attributes.clone()], false);
        }
        new.borrow_mut().attributes = attributes;
        for member in &class.members {
            let attrs = match &member.attributes {
                Some(a) => Some(self.eval(host, a)?),
                None => None,
            };
            let k = self.eval(host, &member.key)?;
            let v = self.eval(host, &member.value)?;
            let new_member = new.borrow().metamethods[MetaMethod::NewMember as usize].clone();
            if !new_member.is_null() {
                let a = attrs.clone().unwrap_or_default();
                if self
                    .call_from_native(host, &new_member, value.clone(), &[k.clone(), v.clone(), a], false)
                    .is_ok()
                {
                    continue;
                }
            }
            self.new_slot(host, &value, k.clone(), v, member.is_static)
                .map_err(|e| self.raise(host, e))?;
            if let Some(attrs) = attrs {
                set_class_attributes(&new, &k, attrs);
            }
        }
        Ok(value)
    }
}

/// `SQClass::SetAttributes`.
pub(super) fn set_class_attributes(class: &ClassRef, key: &Value, attrs: Value) -> bool {
    let mut c = class.borrow_mut();
    match c.members.get(key) {
        Some(Value::Integer(idx)) if idx & MEMBER_FIELD != 0 => {
            c.default_values[(idx & 0x00FF_FFFF) as usize].attrs = attrs;
            true
        }
        Some(Value::Integer(idx)) => {
            c.methods[(idx & 0x00FF_FFFF) as usize].attrs = attrs;
            true
        }
        _ => false,
    }
}

/// `SQClass::GetAttributes`.
pub(super) fn get_class_attributes(class: &ClassRef, key: &Value) -> Option<Value> {
    let c = class.borrow();
    match c.members.get(key)? {
        Value::Integer(idx) if idx & MEMBER_FIELD != 0 => {
            Some(c.default_values[(idx & 0x00FF_FFFF) as usize].attrs.clone())
        }
        Value::Integer(idx) => Some(c.methods[(idx & 0x00FF_FFFF) as usize].attrs.clone()),
        _ => None,
    }
}

/// How a closure call failed: before its frame existed (the caller raises
/// it) or inside (already raised).
pub(super) enum StartCallError {
    StartCall(Value),
    Raised(Value),
}
use StartCallError::{Raised, StartCall};

impl From<StartCallError> for Value {
    fn from(e: StartCallError) -> Value {
        match e {
            StartCall(v) | Raised(v) => v,
        }
    }
}
