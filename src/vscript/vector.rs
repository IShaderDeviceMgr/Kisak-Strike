//! Valve's `Vector` class (`vsquirrel.cpp:140-545`) — a Squirrel class whose
//! instances carry three floats, with `_get`/`_set` for `.x`/`.y`/`.z` and
//! `_add`/`_sub`/`_mul` for arithmetic. It is what `GetOrigin()` returns and
//! what `SetOrigin()` takes.
//!
//! Two of Valve's quirks are kept: `ToKVString` ends in a stray `))`, and
//! `Norm` is `mathlib_base.cpp`'s `VectorNormalize`, which divides by
//! `length + FLT_EPSILON` rather than testing for zero.

use std::any::Any;

use super::value::*;
use super::Vm;

/// `TYPETAG_VECTOR`.
pub const VECTOR_TYPE_TAG: usize = 1;

type R = Result<Value, Value>;

fn err(message: &str) -> Value {
    Value::str(message)
}

/// The three floats a `Vector` instance carries, if `v` is one.
pub fn vector_of(v: &Value) -> Option<[f32; 3]> {
    let Value::Instance(i) = v else {
        return None;
    };
    let i = i.borrow();
    i.user.as_ref()?.downcast_ref::<[f32; 3]>().copied()
}

/// A new `Vector` instance — what a native returning `FIELD_VECTOR` pushes.
pub fn vector_value(vm: &Vm, v: [f32; 3]) -> Value {
    let class = vm
        .vector_class
        .clone()
        .expect("Vector is registered by Vm::new");
    Value::Instance(vm.new_instance(&class, Some(Box::new(v))))
}

fn this_vector(a: &[Value]) -> Result<[f32; 3], Value> {
    a.first().and_then(vector_of).ok_or_else(|| err("null vector"))
}

fn other_vector(a: &[Value]) -> Result<[f32; 3], Value> {
    a.get(1).and_then(vector_of).ok_or_else(|| err("null vector"))
}

/// `%f`.
fn f(v: f32) -> String {
    format!("{:.6}", v as f64)
}

pub(super) fn register(vm: &mut Vm) {
    let class = vm.new_class(None, VECTOR_TYPE_TAG);
    let funcs: &[(&str, i32, &str, fn(&mut Vm, &mut dyn Any, &[Value]) -> R)] = &[
        ("constructor", 0, "", |_, _, a| {
            let mut v = [0.0f32; 3];
            for (i, slot) in v.iter_mut().enumerate() {
                // `sa.GetFloat( i + 2 )` — anything not a number reads as 0.
                *slot = a.get(i + 1).filter(|x| x.is_numeric()).map_or(0.0, Value::to_float);
            }
            if let Some(Value::Instance(i)) = a.first() {
                i.borrow_mut().user = Some(Box::new(v));
            }
            Ok(Value::Null)
        }),
        ("_get", 2, "..", |_, _, a| {
            let v = this_vector(a)?;
            match a.get(1) {
                Some(Value::String(key)) if key.len() == 1 => {
                    let index = key.as_bytes()[0] as i32 - b'x' as i32;
                    match (0..=2).contains(&index) {
                        true => Ok(Value::Float(v[index as usize])),
                        false => Err(Value::Null),
                    }
                }
                _ => Err(Value::Null),
            }
        }),
        ("_set", 3, "..n", |_, _, a| {
            let Some(Value::Instance(inst)) = a.first() else {
                return Err(err("null vector"));
            };
            if vector_of(&a[0]).is_none() {
                return Err(err("null vector"));
            }
            match a.get(1) {
                Some(Value::String(key)) if key.len() == 1 => {
                    let index = key.as_bytes()[0] as i32 - b'x' as i32;
                    if !(0..=2).contains(&index) {
                        return Err(Value::Null);
                    }
                    let value = a.get(2).map_or(0.0, Value::to_float);
                    let mut i = inst.borrow_mut();
                    if let Some(v) = i.user.as_mut().and_then(|u| u.downcast_mut::<[f32; 3]>()) {
                        v[index as usize] = value;
                    }
                    Ok(Value::Null)
                }
                _ => Err(Value::Null),
            }
        }),
        ("_tostring", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            Ok(Value::str(&format!("(vector : ({}, {}, {}))", f(v[0]), f(v[1]), f(v[2]))))
        }),
        ("_typeof", 0, "", |_, _, _| Ok(Value::str("Vector"))),
        ("_nexti", 0, "", |_, _, a| {
            let key = match a.get(1) {
                Some(Value::Null) | None => b'w',
                Some(Value::String(s)) if s.len() == 1 => s.as_bytes()[0],
                _ => return Err(Value::Null),
            };
            Ok(match key as i32 - b'x' as i32 + 1 {
                0 => Value::str("x"),
                1 => Value::str("y"),
                2 => Value::str("z"),
                _ => Value::Null,
            })
        }),
        ("_add", 2, "", |vm, _, a| {
            let (x, y) = (this_vector(a)?, other_vector(a)?);
            Ok(vector_value(vm, [x[0] + y[0], x[1] + y[1], x[2] + y[2]]))
        }),
        ("_sub", 2, "", |vm, _, a| {
            let (x, y) = (this_vector(a)?, other_vector(a)?);
            Ok(vector_value(vm, [x[0] - y[0], x[1] - y[1], x[2] - y[2]]))
        }),
        ("_mul", 2, "", |vm, _, a| {
            let x = this_vector(a)?;
            let s = a.get(1).filter(|v| v.is_numeric()).map_or(0.0, Value::to_float);
            Ok(vector_value(vm, [x[0] * s, x[1] * s, x[2] * s]))
        }),
        ("ToKVString", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            Ok(Value::str(&format!("{} {} {}))", f(v[0]), f(v[1]), f(v[2]))))
        }),
        ("Length", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            Ok(Value::Float((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()))
        }),
        ("LengthSqr", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            Ok(Value::Float(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]))
        }),
        ("Length2D", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            Ok(Value::Float((v[0] * v[0] + v[1] * v[1]).sqrt()))
        }),
        ("Length2DSqr", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            Ok(Value::Float(v[0] * v[0] + v[1] * v[1]))
        }),
        ("Dot", 2, "", |_, _, a| {
            let (x, y) = (this_vector(a)?, other_vector(a)?);
            Ok(Value::Float(x[0] * y[0] + x[1] * y[1] + x[2] * y[2]))
        }),
        ("Cross", 2, "", |vm, _, a| {
            let (x, y) = (this_vector(a)?, other_vector(a)?);
            Ok(vector_value(
                vm,
                [
                    x[1] * y[2] - x[2] * y[1],
                    x[2] * y[0] - x[0] * y[2],
                    x[0] * y[1] - x[1] * y[0],
                ],
            ))
        }),
        ("Norm", 0, "", |_, _, a| {
            let v = this_vector(a)?;
            let radius = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let inverse = 1.0 / (radius + f32::EPSILON);
            if let Some(Value::Instance(i)) = a.first() {
                if let Some(v) = i.borrow_mut().user.as_mut().and_then(|u| u.downcast_mut::<[f32; 3]>()) {
                    v.iter_mut().for_each(|c| *c *= inverse);
                }
            }
            Ok(Value::Float(radius))
        }),
    ];
    for (name, nparams, mask, func) in funcs {
        let native = vm.native(name, *nparams, mask, *func);
        vm.class_new_slot(&class, name, native);
    }
    let root = vm.root();
    vm.set_slot(&root, "Vector", Value::Class(class.clone()));
    vm.vector_class = Some(class);
}
