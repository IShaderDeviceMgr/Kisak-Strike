//! `sqbaselib.cpp` — the root table's functions and every type's default
//! delegate (`len`, `slice`, `find`, `call`, `bindenv`, …).
//!
//! A native returning `SQ_OK` without pushing anything returns `null`, which
//! is why `append` and `rawset` do.

use std::any::Any;
use std::rc::Rc;

use super::interp::{class_get, create_instance, get_class_attributes, set_class_attributes};
use super::lexer::strtod_prefix;
use super::value::*;
use super::Vm;

type R = Result<Value, Value>;

fn err(message: &str) -> Value {
    Value::str(message)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

pub(super) fn register(vm: &mut Vm) {
    let root = vm.root();
    vm.register_native(&root, "seterrorhandler", 2, "", |vm, _, a| {
        vm.error_handler = arg(a, 1);
        Ok(Value::Null)
    });
    vm.register_native(&root, "setdebughook", 2, "", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "enabledebuginfo", 2, "", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "getstackinfos", 2, ".n", |vm, _, a| {
        // Level 0 is `getstackinfos` itself.
        let level = arg(a, 1).to_integer().max(0) as usize;
        let Some((func, src, line)) = vm.stack_infos(level) else {
            return Ok(Value::Null);
        };
        let t = vm.new_table(4);
        let locals = vm.new_table(0);
        for (name, value) in vm.stack_locals(level) {
            locals.borrow_mut().table.new_slot(Value::str(&name), value);
        }
        {
            let mut t = t.borrow_mut();
            t.table.new_slot(Value::str("func"), Value::str(&func));
            t.table.new_slot(Value::str("src"), Value::str(&src));
            t.table.new_slot(Value::str("line"), Value::Integer(line));
            t.table.new_slot(Value::str("locals"), Value::Table(locals));
        }
        Ok(Value::Table(t))
    });
    vm.register_native(&root, "getroottable", 1, "", |vm, _, _| Ok(Value::Table(vm.root())));
    vm.register_native(&root, "setroottable", 2, "", |vm, _, a| match arg(a, 1) {
        Value::Table(t) => {
            vm.root = t;
            Ok(Value::Null)
        }
        _ => Err(err("ivalid type")),
    });
    vm.register_native(&root, "getconsttable", 1, "", |vm, _, _| Ok(Value::Table(vm.consts())));
    vm.register_native(&root, "setconsttable", 2, "", |vm, _, a| match arg(a, 1) {
        Value::Table(t) => {
            vm.consts = t;
            Ok(Value::Null)
        }
        _ => Err(err("ivalid type")),
    });
    vm.register_native(&root, "assert", 2, "", |_, _, a| match arg(a, 1).is_false() {
        true => Err(err("assertion failed")),
        false => Ok(Value::Null),
    });
    vm.register_native(&root, "print", 2, "", |vm, host, a| {
        let text = vm.value_to_string(host, &arg(a, 1));
        vm.print(&text.to_string_lossy());
        Ok(Value::Null)
    });
    vm.register_native(&root, "compilestring", -2, ".ss", |vm, _, a| {
        let source = arg(a, 1);
        let name = match a.get(2) {
            Some(Value::String(s)) => s.to_string_lossy(),
            _ => "unnamedbuffer".into(),
        };
        let bytes = source.as_string().map(|s| s.as_bytes().to_vec()).unwrap_or_default();
        vm.compile(&bytes, &name)
            .map_err(|e| err(&e.message))
    });
    vm.register_native(&root, "newthread", 2, ".c", |_, _, _| {
        Err(err("threads are not supported by this VM (no shipped script uses newthread)"))
    });
    vm.register_native(&root, "suspend", -1, "", |_, _, _| {
        Err(err("cannot suspend through native calls/metamethods"))
    });
    vm.register_native(&root, "array", -2, ".n", |vm, _, a| {
        let size = arg(a, 1).to_integer().max(0) as usize;
        let fill = if a.len() > 2 { arg(a, 2) } else { Value::Null };
        Ok(Value::Array(vm.new_array(vec![fill; size])))
    });
    vm.register_native(&root, "type", 2, "", |_, _, a| Ok(Value::str(arg(a, 1).type_name())));
    vm.register_native(&root, "dummy", 0, "", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "collectgarbage", 1, "t", |_, _, _| Ok(Value::Integer(0)));
    vm.set_slot(&root, "_version_", Value::str("Squirrel 2.2.3 stable"));
    vm.set_slot(&root, "_charsize_", Value::Integer(1));
    vm.set_slot(&root, "_intsize_", Value::Integer(4));
    vm.set_slot(&root, "_floatsize_", Value::Integer(4));

    register_table_delegate(vm);
    register_array_delegate(vm);
    register_string_delegate(vm);
    register_number_delegate(vm);
    register_closure_delegate(vm);
    register_class_delegate(vm);
    register_instance_delegate(vm);
    register_weakref_delegate(vm);
}

/// `sq_getsize`.
fn size_of(v: &Value) -> Result<i32, Value> {
    match v {
        Value::String(s) => Ok(s.len() as i32),
        Value::Table(t) => Ok(t.borrow().table.len() as i32),
        Value::Array(a) => Ok(a.borrow().len() as i32),
        _ => Err(err("the object doesn't have a size")),
    }
}

fn len(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    size_of(&arg(a, 0)).map(Value::Integer)
}

fn tostring(vm: &mut Vm, host: &mut dyn Any, a: &[Value]) -> R {
    Ok(Value::String(vm.value_to_string(host, &arg(a, 0))))
}

fn weakref(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    Ok(arg(a, 0).weak())
}

/// `sq_clear`.
fn clear(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    match arg(a, 0) {
        Value::Table(t) => t.borrow_mut().table.clear(),
        Value::Array(v) => v.borrow_mut().clear(),
        _ => return Err(err("clear only works on table and array")),
    }
    Ok(Value::Null)
}

/// `sq_rawget` — own slots only, for a table.
fn raw_get(vm: &mut Vm, host: &mut dyn Any, object: &Value, key: &Value) -> Option<Value> {
    match object {
        Value::Table(t) => t.borrow().table.get(key),
        Value::Class(c) => class_get(c, key),
        Value::Instance(_) | Value::Array(_) => vm.get_value(host, object, key, true, false),
        _ => None,
    }
}

fn rawin(vm: &mut Vm, host: &mut dyn Any, a: &[Value]) -> R {
    Ok(Value::Bool(raw_get(vm, host, &arg(a, 0), &arg(a, 1)).is_some()))
}

fn register_table_delegate(vm: &mut Vm) {
    let d = vm.delegates.table.clone();
    vm.register_native(&d, "len", 1, "t", len);
    vm.register_native(&d, "rawget", 2, "t", |vm, host, a| {
        raw_get(vm, host, &arg(a, 0), &arg(a, 1)).ok_or_else(|| err("the index doesn't exist"))
    });
    vm.register_native(&d, "rawset", 3, "t", |_, _, a| {
        let key = arg(a, 1);
        if key.is_null() {
            return Err(err("null key"));
        }
        if let Value::Table(t) = arg(a, 0) {
            t.borrow_mut().table.new_slot(key, arg(a, 2));
        }
        Ok(Value::Null)
    });
    vm.register_native(&d, "rawdelete", 2, "t", |_, _, a| {
        let Value::Table(t) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let key = arg(a, 1);
        let old = t.borrow().table.get(&key);
        match old {
            Some(old) => {
                t.borrow_mut().table.remove(&key);
                Ok(old)
            }
            None => Ok(Value::Null),
        }
    });
    vm.register_native(&d, "rawin", 2, "t", rawin);
    vm.register_native(&d, "weakref", 1, "", weakref);
    vm.register_native(&d, "tostring", 1, ".", tostring);
    vm.register_native(&d, "clear", 1, ".", clear);
}

/// `get_slice_params`.
fn slice_params(a: &[Value], size: i32) -> (i32, i32) {
    let start = match a.get(1) {
        Some(v) if v.is_numeric() => v.to_integer(),
        _ => 0,
    };
    let end = if a.len() > 2 {
        match a.get(2) {
            Some(v) if v.is_numeric() => v.to_integer(),
            _ => 0,
        }
    } else {
        size
    };
    (start, end)
}

/// `_qsort_compare`.
fn sort_compare(vm: &mut Vm, host: &mut dyn Any, func: &Option<Value>, a: &Value, b: &Value) -> Result<i32, Value> {
    match func {
        None => vm.compare(host, a, b),
        Some(f) => {
            let root = Value::Table(vm.root());
            match vm.call(host, f, root, &[a.clone(), b.clone()], false) {
                // A comparator that returns something other than a number
                // leaves `ret` uninitialised in the C; zero is the kindest
                // reading of that.
                Ok(v) => Ok(if v.is_numeric() { v.to_integer() } else { 0 }),
                Err(e) => Err(match e {
                    Value::String(_) => e,
                    _ => err("compare func failed"),
                }),
            }
        }
    }
}

/// A swap that tolerates a comparator having shrunk the array under the sort.
fn swap(arr: &ArrayRef, i: i32, j: i32) {
    let mut a = arr.borrow_mut();
    if (i as usize) < a.len() && (j as usize) < a.len() {
        a.swap(i as usize, j as usize);
    }
}

/// `_qsort` — Sedgewick's quicksort, kept exactly, because a comparator
/// that is not a total order makes the result depend on the algorithm.
fn qsort(vm: &mut Vm, host: &mut dyn Any, arr: &ArrayRef, l: i32, r: i32, func: &Option<Value>) -> Result<(), Value> {
    if l >= r {
        return Ok(());
    }
    let get = |i: i32| arr.borrow().get(i as usize).cloned().unwrap_or_default();
    let pivot = get(l);
    let mut i = l;
    let mut j = r + 1;
    loop {
        let mut ret;
        loop {
            i += 1;
            if i > r {
                break;
            }
            ret = sort_compare(vm, host, func, &get(i), &pivot)?;
            if ret > 0 {
                break;
            }
        }
        loop {
            j -= 1;
            if j < 0 {
                return Err(err("Invalid qsort, probably compare function defect"));
            }
            ret = sort_compare(vm, host, func, &get(j), &pivot)?;
            if ret <= 0 {
                break;
            }
        }
        if i >= j {
            break;
        }
        swap(arr, i, j);
    }
    swap(arr, l, j);
    qsort(vm, host, arr, l, j - 1, func)?;
    qsort(vm, host, arr, j + 1, r, func)
}

fn register_array_delegate(vm: &mut Vm) {
    let d = vm.delegates.array.clone();
    vm.register_native(&d, "len", 1, "a", len);
    let append = |_: &mut Vm, _: &mut dyn Any, a: &[Value]| -> R {
        if let Value::Array(v) = arg(a, 0) {
            v.borrow_mut().push(arg(a, 1));
        }
        Ok(Value::Null)
    };
    vm.register_native(&d, "append", 2, "a", append);
    vm.register_native(&d, "push", 2, "a", append);
    vm.register_native(&d, "extend", 2, "aa", |_, _, a| {
        if let (Value::Array(x), Value::Array(y)) = (arg(a, 0), arg(a, 1)) {
            let more = y.borrow().clone();
            x.borrow_mut().extend(more);
        }
        Ok(Value::Null)
    });
    vm.register_native(&d, "pop", 1, "a", |_, _, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let popped = v.borrow_mut().pop();
        popped.map(|v| v.real()).ok_or_else(|| err("empty array"))
    });
    vm.register_native(&d, "top", 1, "a", |_, _, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let top = v.borrow().last().cloned();
        top.ok_or_else(|| err("top() on a empty array"))
    });
    vm.register_native(&d, "insert", 3, "an", |_, _, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let i = arg(a, 1).to_integer();
        let mut v = v.borrow_mut();
        if i < 0 || i as usize > v.len() {
            return Err(err("index out of range"));
        }
        v.insert(i as usize, arg(a, 2));
        Ok(Value::Null)
    });
    vm.register_native(&d, "remove", 2, "an", |_, _, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let idx = arg(a, 1);
        if !idx.is_numeric() {
            return Err(err("wrong type"));
        }
        let i = idx.to_integer();
        let mut v = v.borrow_mut();
        if i < 0 || i as usize >= v.len() {
            return Err(err("idx out of range"));
        }
        Ok(v.remove(i as usize).real())
    });
    vm.register_native(&d, "resize", -2, "an", |_, _, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let size = arg(a, 1);
        if !size.is_numeric() {
            return Err(err("size must be a number"));
        }
        let fill = if a.len() > 2 { arg(a, 2) } else { Value::Null };
        v.borrow_mut().resize(size.to_integer().max(0) as usize, fill);
        Ok(Value::Null)
    });
    vm.register_native(&d, "reverse", 1, "a", |_, _, a| {
        if let Value::Array(v) = arg(a, 0) {
            v.borrow_mut().reverse();
        }
        Ok(Value::Null)
    });
    vm.register_native(&d, "sort", -1, "ac", |vm, host, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let size = v.borrow().len() as i32;
        if size > 1 {
            let func = match a.get(1) {
                Some(f @ (Value::Closure(_) | Value::Native(_))) => Some(f.clone()),
                _ => None,
            };
            qsort(vm, host, &v, 0, size - 1, &func)?;
        }
        Ok(Value::Null)
    });
    vm.register_native(&d, "slice", -1, "ann", |vm, _, a| {
        let Value::Array(v) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let len = v.borrow().len() as i32;
        let (mut s, mut e) = slice_params(a, len);
        if s < 0 {
            s += len;
        }
        if e < 0 {
            e += len;
        }
        if e < s {
            return Err(err("wrong indexes"));
        }
        if e > len {
            return Err(err("slice out of range"));
        }
        let items: Vec<Value> = (s..e)
            .map(|i| v.borrow().get(i as usize).map(|x| x.real()).unwrap_or_default())
            .collect();
        Ok(Value::Array(vm.new_array(items)))
    });
    vm.register_native(&d, "weakref", 1, "", weakref);
    vm.register_native(&d, "tostring", 1, ".", tostring);
    vm.register_native(&d, "clear", 1, ".", clear);
}

/// `str2num` — a `.` makes it a float (`strtod`), otherwise `strtol` in
/// base 10, and either reads the longest prefix that parses.
pub(super) fn str2num(s: &[u8]) -> Option<Value> {
    if s.contains(&b'.') {
        let text = String::from_utf8_lossy(s);
        let trimmed = text.trim_start();
        let v = strtod_prefix(trimmed.as_bytes());
        // `s == end` — nothing parsed at all.
        let starts_like_a_number = trimmed
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == b'.' || c == b'-' || c == b'+');
        let any_digit = trimmed.bytes().take_while(|c| !c.is_ascii_whitespace()).any(|c| c.is_ascii_digit());
        if !starts_like_a_number || !any_digit {
            return None;
        }
        return Some(Value::Float(v as f32));
    }
    let text = String::from_utf8_lossy(s);
    let t = text.trim_start();
    let (negative, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let run: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
    if run.is_empty() {
        return None;
    }
    // `strtol` clamps to `LONG_MAX`/`LONG_MIN`, and `long` is 32 bits on the
    // platform the game shipped on.
    let magnitude: i64 = run.parse::<i64>().unwrap_or(i64::MAX);
    let value = if negative { -magnitude } else { magnitude };
    Some(Value::Integer(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32))
}

fn to_integer(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    match arg(a, 0) {
        Value::String(s) => str2num(s.as_bytes())
            .map(|v| Value::Integer(v.to_integer()))
            .ok_or_else(|| err("cannot convert the string")),
        v @ (Value::Integer(_) | Value::Float(_)) => Ok(Value::Integer(v.to_integer())),
        Value::Bool(b) => Ok(Value::Integer(b as i32)),
        _ => Ok(Value::Null),
    }
}

fn to_float(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    match arg(a, 0) {
        Value::String(s) => str2num(s.as_bytes())
            .map(|v| Value::Float(v.to_float()))
            .ok_or_else(|| err("cannot convert the string")),
        v @ (Value::Integer(_) | Value::Float(_)) => Ok(Value::Float(v.to_float())),
        Value::Bool(b) => Ok(Value::Float(if b { 1.0 } else { 0.0 })),
        _ => Ok(Value::Null),
    }
}

fn register_string_delegate(vm: &mut Vm) {
    let d = vm.delegates.string.clone();
    vm.register_native(&d, "len", 1, "s", len);
    vm.register_native(&d, "tointeger", 1, "s", to_integer);
    vm.register_native(&d, "tofloat", 1, "s", to_float);
    vm.register_native(&d, "tostring", 1, ".", tostring);
    vm.register_native(&d, "slice", -1, " s n  n", |_, _, a| {
        let Value::String(s) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let len = s.len() as i32;
        let (mut start, mut end) = slice_params(a, len);
        if start < 0 {
            start += len;
        }
        if end < 0 {
            end += len;
        }
        if end < start {
            return Err(err("wrong indexes"));
        }
        if end > len {
            return Err(err("slice out of range"));
        }
        let start = start.max(0) as usize;
        Ok(Value::bytes(&s.as_bytes()[start..end as usize]))
    });
    vm.register_native(&d, "find", -2, "s s n ", |_, _, a| {
        let (Value::String(s), Value::String(sub)) = (arg(a, 0), arg(a, 1)) else {
            return Err(err("invalid param"));
        };
        let start = if a.len() > 2 { arg(a, 2).to_integer() } else { 0 };
        if (s.len() as i32) > start && start >= 0 {
            let hay = &s.as_bytes()[start as usize..];
            // `strstr` stops at a NUL in either string.
            let needle = sub.as_bytes().split(|&c| c == 0).next().unwrap_or(&[]);
            let hay = hay.split(|&c| c == 0).next().unwrap_or(&[]);
            if needle.is_empty() {
                return Ok(Value::Integer(start));
            }
            if let Some(pos) = hay.windows(needle.len()).position(|w| w == needle) {
                return Ok(Value::Integer(start + pos as i32));
            }
        }
        Ok(Value::Null)
    });
    vm.register_native(&d, "tolower", 1, "s", |_, _, a| {
        let Value::String(s) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        Ok(Value::bytes(&s.as_bytes().to_ascii_lowercase()))
    });
    vm.register_native(&d, "toupper", 1, "s", |_, _, a| {
        let Value::String(s) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        Ok(Value::bytes(&s.as_bytes().to_ascii_uppercase()))
    });
    vm.register_native(&d, "weakref", 1, "", weakref);
}

fn register_number_delegate(vm: &mut Vm) {
    let d = vm.delegates.number.clone();
    vm.register_native(&d, "tointeger", 1, "n|b", to_integer);
    vm.register_native(&d, "tofloat", 1, "n|b", to_float);
    vm.register_native(&d, "tostring", 1, ".", tostring);
    vm.register_native(&d, "tochar", 1, "n|b", |_, _, a| {
        Ok(Value::bytes(&[arg(a, 0).to_integer() as u8]))
    });
    vm.register_native(&d, "weakref", 1, "", weakref);
}

fn register_closure_delegate(vm: &mut Vm) {
    let d = vm.delegates.closure.clone();
    // `f.call( this, args... )`.
    vm.register_native(&d, "call", -1, "c", |vm, host, a| {
        let f = arg(a, 0);
        let this = arg(a, 1);
        let rest = if a.len() > 2 { &a[2..] } else { &[] };
        if a.len() < 2 {
            return Err(err("wrong number of parameters"));
        }
        vm.call(host, &f, this, rest, true)
    });
    vm.register_native(&d, "pcall", -1, "c", |vm, host, a| {
        let f = arg(a, 0);
        if a.len() < 2 {
            return Err(err("wrong number of parameters"));
        }
        let rest = if a.len() > 2 { &a[2..] } else { &[] };
        vm.call(host, &f, arg(a, 1), rest, false)
    });
    let acall = |raise: bool| {
        move |vm: &mut Vm, host: &mut dyn Any, a: &[Value]| -> R {
            let f = arg(a, 0);
            let Value::Array(params) = arg(a, 1) else {
                return Ok(Value::Null);
            };
            let params = params.borrow().clone();
            let Some((this, rest)) = params.split_first() else {
                return Err(err("wrong number of parameters"));
            };
            vm.call(host, &f, this.clone(), rest, raise)
        }
    };
    vm.register_native(&d, "acall", 2, "ca", acall(true));
    vm.register_native(&d, "pacall", 2, "ca", acall(false));
    vm.register_native(&d, "weakref", 1, "", weakref);
    vm.register_native(&d, "tostring", 1, ".", tostring);
    // `sq_bindenv` — a copy of the closure whose `this` is pinned, weakly.
    vm.register_native(&d, "bindenv", 2, "c x|y|t", |_, _, a| {
        let env = match arg(a, 1).weak() {
            Value::WeakRef(w) => (*w).clone(),
            _ => return Err(err("invalid environment")),
        };
        match arg(a, 0) {
            Value::Closure(c) => Ok(Value::Closure(Rc::new(Closure {
                proto: c.proto.clone(),
                outers: c.outers.clone(),
                defaults: c.defaults.clone(),
                env: Some(env),
            }))),
            Value::Native(n) => Ok(Value::Native(Rc::new(Native {
                name: n.name.clone(),
                func: n.func.clone(),
                nparamscheck: n.nparamscheck,
                typecheck: n.typecheck.clone(),
                env: Some(env),
            }))),
            _ => Err(err("the target is not a closure")),
        }
    });
    vm.register_native(&d, "getinfos", 1, "c", |vm, _, a| {
        let res = vm.new_table(4);
        match arg(a, 0) {
            Value::Closure(c) => {
                let p = &c.proto;
                let mut params: Vec<Value> = p.params.iter().map(|s| Value::String(s.clone())).collect();
                if p.varparams {
                    params.push(Value::str("..."));
                }
                let params = Value::Array(vm.new_array(params));
                let mut t = res.borrow_mut();
                t.table.new_slot(Value::str("native"), Value::Bool(false));
                t.table.new_slot(Value::str("name"), p.name.clone());
                t.table.new_slot(Value::str("src"), Value::String(p.source.clone()));
                t.table.new_slot(Value::str("parameters"), params);
                t.table.new_slot(Value::str("varargs"), Value::Bool(p.varparams));
            }
            Value::Native(n) => {
                let typecheck = match n.typecheck.is_empty() {
                    true => Value::Null,
                    false => Value::Array(vm.new_array(
                        n.typecheck.iter().map(|m| Value::Integer(*m as i32)).collect(),
                    )),
                };
                let mut t = res.borrow_mut();
                t.table.new_slot(Value::str("native"), Value::Bool(true));
                t.table.new_slot(Value::str("name"), Value::String(n.name.clone()));
                t.table.new_slot(Value::str("paramscheck"), Value::Integer(n.nparamscheck));
                t.table.new_slot(Value::str("typecheck"), typecheck);
            }
            _ => {}
        }
        Ok(Value::Table(res))
    });
}

fn register_class_delegate(vm: &mut Vm) {
    let d = vm.delegates.class.clone();
    vm.register_native(&d, "getattributes", 2, "y.", |_, _, a| {
        let Value::Class(c) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let key = arg(a, 1);
        if key.is_null() {
            return Ok(c.borrow().attributes.clone());
        }
        get_class_attributes(&c, &key).ok_or_else(|| err("wrong index"))
    });
    vm.register_native(&d, "setattributes", 3, "y..", |_, _, a| {
        let Value::Class(c) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let key = arg(a, 1);
        let value = arg(a, 2);
        if key.is_null() {
            let old = std::mem::replace(&mut c.borrow_mut().attributes, value);
            return Ok(old);
        }
        let old = get_class_attributes(&c, &key);
        match old {
            Some(old) => {
                set_class_attributes(&c, &key, value);
                Ok(old)
            }
            None => Err(err("wrong index")),
        }
    });
    vm.register_native(&d, "rawin", 2, "y", rawin);
    vm.register_native(&d, "weakref", 1, "", weakref);
    vm.register_native(&d, "tostring", 1, ".", tostring);
    vm.register_native(&d, "instance", 1, "y", |vm, _, a| {
        let Value::Class(c) = arg(a, 0) else {
            return Ok(Value::Null);
        };
        let instance = create_instance(&c);
        vm.track(Rc::downgrade(&instance) as std::rc::Weak<dyn super::Finalize>);
        Ok(Value::Instance(instance))
    });
}

fn register_instance_delegate(vm: &mut Vm) {
    let d = vm.delegates.instance.clone();
    vm.register_native(&d, "getclass", 1, "x", |_, _, a| match arg(a, 0) {
        Value::Instance(i) => Ok(Value::Class(i.borrow().class.clone())),
        _ => Err(err("the object is not a class instance")),
    });
    vm.register_native(&d, "rawin", 2, "x", rawin);
    vm.register_native(&d, "weakref", 1, "", weakref);
    vm.register_native(&d, "tostring", 1, ".", tostring);
}

fn register_weakref_delegate(vm: &mut Vm) {
    let d = vm.delegates.weakref.clone();
    vm.register_native(&d, "ref", 1, "r", |_, _, a| Ok(arg(a, 0).real()));
    vm.register_native(&d, "weakref", 1, "", weakref);
    vm.register_native(&d, "tostring", 1, ".", tostring);
}

/// `CSquirrelVM::GetFunctionSignature` (`vsquirrel.cpp`) — what `PrintHelp`
/// shows for a script function.
pub(super) fn get_function_signature(_: &mut Vm, _: &mut dyn Any, a: &[Value]) -> R {
    if a.len() != 3 {
        return Ok(Value::Null);
    }
    let Value::Closure(c) = arg(a, 1) else {
        return Ok(Value::Null);
    };
    let mut out = String::from("function ");
    match (arg(a, 2), &c.proto.name) {
        (Value::String(name), _) if name.len() > 0 => out.push_str(&name.to_string_lossy()),
        (_, Value::String(name)) => out.push_str(&name.to_string_lossy()),
        _ => out.push_str("<unnamed>"),
    }
    out.push('(');
    for (i, p) in c.proto.params.iter().enumerate().skip(1) {
        if i != 1 {
            out.push_str(", ");
        }
        out.push_str(&p.to_string_lossy());
    }
    out.push(')');
    Ok(Value::str(&out))
}
