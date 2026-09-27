//! The language, tested against what Squirrel 2.2's `sqvm.cpp` and
//! `sqcompiler.cpp` do. Every expected value here is derived from the C, and
//! the ones that would surprise a reader say where from.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;

/// A VM whose `print` goes into a buffer, and the buffer.
fn vm() -> (Vm, Rc<RefCell<String>>) {
    let out = Rc::new(RefCell::new(String::new()));
    let sink = out.clone();
    let vm = Vm::new(0, Box::new(move |text| sink.borrow_mut().push_str(text)));
    out.borrow_mut().clear();
    (vm, out)
}

/// Runs `src` as a main chunk and returns what it `return`ed.
fn eval(src: &str) -> Value {
    let (mut vm, out) = vm();
    let f = vm.compile(src.as_bytes(), "test").expect("compiles");
    match vm.execute(&mut (), &f, None, &[]) {
        Ok(v) => v,
        Err(e) => panic!("script raised {e:?}; printed:\n{}", out.borrow()),
    }
}

/// Runs `src` and returns everything it printed.
fn output(src: &str) -> String {
    let (mut vm, out) = vm();
    let f = vm.compile(src.as_bytes(), "test").expect("compiles");
    let _ = vm.execute(&mut (), &f, None, &[]);
    let text = out.borrow().clone();
    text
}

fn raised(src: &str) -> String {
    let (mut vm, _) = vm();
    let f = vm.compile(src.as_bytes(), "test").expect("compiles");
    match vm.execute(&mut (), &f, None, &[]) {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e.to_display_string(),
    }
}

fn compile_error(src: &str) -> String {
    let (mut vm, _) = vm();
    match vm.compile(src.as_bytes(), "test") {
        Ok(_) => panic!("expected a compile error"),
        Err(e) => e.message,
    }
}

fn int(v: Value) -> i32 {
    match v {
        Value::Integer(i) => i,
        other => panic!("expected an integer, got {other:?}"),
    }
}

fn string(v: Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn integers_are_32_bit_and_wrap() {
    assert_eq!(int(eval("return 2147483647 + 1")), i32::MIN);
    assert_eq!(int(eval("return 7 / 2")), 3);
    assert_eq!(int(eval("return -7 / 2")), -3);
    assert_eq!(int(eval("return -7 % 3")), -1);
    assert_eq!(int(eval("return 1 << 33")), 2);
    assert_eq!(int(eval("return -8 >>> 28")), 15);
    assert_eq!(int(eval("return 0x10 | 010")), 24);
}

#[test]
fn floats_are_single_precision_and_print_with_percent_g() {
    assert_eq!(string(eval("return \"\" + 0.1")), "0.1");
    assert_eq!(string(eval("return \"\" + (1.0 / 3)")), "0.333333");
    assert_eq!(string(eval("return \"\" + 1.0")), "1");
    assert_eq!(string(eval("return \"\" + (1.0e10).tointeger()")), "-2147483648");
    // `ReadNumber` takes every `.` it meets, so a method call straight off a
    // float literal is a compile error in Squirrel too.
    assert_eq!(compile_error("return 1.5.tostring()"), "end of statement expected (; or lf)");
    assert_eq!(compile_error("return 1e10"), "invalid numeric format");
    assert_eq!(string(eval("return (7.5 % 2).tostring()")), "1.5");
}

#[test]
fn mixed_arithmetic_and_string_concatenation() {
    assert_eq!(string(eval("return 1 + 2.5 + \"x\"")), "3.5x");
    assert_eq!(string(eval("return \"a\" + null")), "a(null : 0x00000000)");
    assert_eq!(string(eval("return \"n\" + true")), "ntrue");
    assert_eq!(raised("return 1 / 0"), "division by zero");
    assert_eq!(raised("return true + 1"), "arith op + on between 'bool' and 'integer' (division by zero)"
        .replace(" (division by zero)", ""));
}

#[test]
fn truthiness_is_null_false_zero_and_zero_point_zero() {
    assert_eq!(
        string(eval(
            "local r = \"\"; foreach (v in [null, false, 0, 0.0, -0.0, \"\", [], 1]) r += (v ? \"t\" : \"f\"); return r"
        )),
        "fffffttt"
    );
}

#[test]
fn equality_is_by_type_then_value_and_floats_compare_bits() {
    assert!(matches!(eval("return 1 == 1.0"), Value::Bool(true)));
    assert!(matches!(eval("return true == 1"), Value::Bool(false)));
    assert!(matches!(eval("return \"a\" == \"a\""), Value::Bool(true)));
    // Same-type floats are equal only if their bits are.
    assert!(matches!(eval("return 0.0 == -0.0"), Value::Bool(false)));
    assert!(matches!(eval("local t = {}\nreturn t == t"), Value::Bool(true)));
    assert!(matches!(eval("return {} == {}"), Value::Bool(false)));
}

#[test]
fn comparison_has_one_precedence_level_with_equality() {
    // `==` and `<` share `CompExp`, left-associative: (1 == 1) < 2 compares a
    // bool with an integer, which is an error.
    assert!(raised("return 1 == 1 < 2").starts_with("comparsion between"));
    assert!(matches!(eval("return null < 1"), Value::Bool(true)));
    assert!(matches!(eval("return \"abc\" < \"abd\""), Value::Bool(true)));
}

#[test]
fn locals_blocks_and_loops() {
    assert_eq!(
        int(eval(
            "local s = 0; for (local i = 0; i < 10; i++) { if (i == 3) continue; if (i == 8) break; s += i } return s"
        )),
        25
    );
    assert_eq!(int(eval("local i = 0; while (i < 5) i++; return i")), 5);
    assert_eq!(int(eval("local i = 0\ndo { i += 2 } while (i < 7)\nreturn i")), 8);
    assert_eq!(int(eval("local x = 1; { local x = 2 } return x")), 1);
}

#[test]
fn a_newline_ends_a_statement_and_blocks_an_index() {
    assert_eq!(int(eval("local a = 1\nlocal b = 2\nreturn a + b")), 3);
    // `a\n[1]` is not an index of `a`: it is refused at compile time.
    assert!(compile_error("local a = [1,2]\nlocal b = a\n[0]").starts_with("cannot brake deref"));
    // …but a call across a newline is still a call.
    assert_eq!(int(eval("function f(x) { return x * 2 }\nreturn f\n(21)")), 42);
    // No separators are needed between arguments or elements.
    assert_eq!(int(eval("local a = [1 2 3]; return a.len()")), 3);
}

#[test]
fn unqualified_names_are_fields_of_this_and_fall_back_to_the_root() {
    let src = "
        g <- 10
        local scope = { v = 5 }
        local f = function() { return v + g }
        return f.call(scope)
    ";
    assert_eq!(int(eval(src)), 15);
    // A missing name is an index error on `this`.
    assert_eq!(raised("return nosuchname"), "the index 'nosuchname' does not exist");
}

#[test]
fn enclosing_locals_are_invisible_without_an_outer_list() {
    // Squirrel 2 has no upvalues: `x` inside is `this.x`.
    assert_eq!(
        raised("local x = 1; local f = function() { return x }\nreturn f()"),
        "the index 'x' does not exist"
    );
    // With `:(x)` it is a copy taken when the closure is made.
    assert_eq!(
        int(eval("local x = 1; local f = function():(x) { return x }\nx = 2; return f()")),
        1
    );
    assert_eq!(
        compile_error("local x = 1; local f = function():(x) { x = 3 }"),
        "free variables cannot be modified"
    );
}

#[test]
fn a_method_call_binds_this_and_a_grouped_one_does_not() {
    let src = "
        name <- \"root\"
        local t = { name = \"t\", who = function() { return name } }
        return t.who() + \",\" + (t.who)()
    ";
    assert_eq!(string(eval(src)), "t,root");
}

#[test]
fn default_parameters_and_varargs() {
    assert_eq!(int(eval("function f(a, b = 10) { return a + b } return f(1) + f(1, 2)")), 14);
    assert_eq!(
        int(eval("function f(a, ...) { local s = a; for (local i = 0; i < vargc; i++) s += vargv[i]; return s } return f(1, 2, 3)")),
        6
    );
    assert_eq!(raised("function f(a) {} f()"), "wrong number of parameters");
    assert_eq!(raised("function f(a) {} f(1, 2)"), "wrong number of parameters");
}

#[test]
fn tables_slots_delegates_and_new_slot() {
    assert_eq!(int(eval("local t = {}\nt.a <- 1; t.a = 2; return t.a")), 2);
    assert_eq!(raised("local t = {}\nt.a = 1"), "the index 'a' does not exist");
    assert_eq!(
        int(eval("local p = { x = 7 }\nlocal c = {}\ndelegate p : c; return c.x")),
        7
    );
    assert!(matches!(eval("local t = { a = 1 }\nreturn \"a\" in t"), Value::Bool(true)));
    assert_eq!(int(eval("local t = { a = 1, b = 2 }\ndelete t.a; return t.len()")), 1);
    assert_eq!(int(eval("local t = { [1] = 5 }\nreturn t[1]")), 5);
}

#[test]
fn table_default_delegate_is_reached_through_a_delegate_chain() {
    // An entity scope delegates to the root; `len` still resolves.
    assert_eq!(int(eval("local root = {}\nlocal s = { a = 1 }\ndelegate root : s; return s.len()")), 1);
}

#[test]
fn classes_instances_and_constructors() {
    let src = "
        class A {
            x = 1
            constructor(v) { x = v }
            function get() { return x }
            static s = 9
        }
        class B extends A {
            function get() { return x * 10 }
        }
        local a = A(3)
        local b = B(4)
        return a.get() + b.get() + (b instanceof A ? 100 : 0) + A.s
    ";
    assert_eq!(int(eval(src)), 3 + 40 + 100 + 9);
    assert_eq!(
        raised("class C { x = 1 } local c = C(); C.y <- 2"),
        "trying to modify a class that has already been instantiated"
    );
    assert_eq!(string(eval("class C {} return typeof C()")), "instance");
}

#[test]
fn metamethods_on_a_delegate_and_a_class() {
    let src = "
        local mt = { _add = function(o) { return 42 }, _get = function(k) { return k + \"!\" } }
        local t = {}
        delegate mt : t
        return (t + 1) + t.hello
    ";
    assert_eq!(string(eval(src)), "42hello!");
    let src = "
        class V { v = 0; constructor(x) { v = x } function _tostring() { return \"V\" + v } }
        return \"\" + V(3)
    ";
    assert_eq!(string(eval(src)), "V3");
}

#[test]
fn try_catch_throw_and_switch() {
    assert_eq!(string(eval("try { throw \"boom\" } catch (e) { return e }")), "boom");
    assert_eq!(
        string(eval("try { local x = {}.missing } catch (e) { return e }")),
        "the index 'missing' does not exist"
    );
    let src = "
        local r = \"\"
        foreach (v in [1, 2, 3, 4]) {
            switch (v) {
                case 1: r += \"a\"
                case 2: r += \"b\"; break
                case 3: r += \"c\"; break
                default: r += \"d\"
            }
        }
        return r
    ";
    assert_eq!(string(eval(src)), "abbcd");
}

#[test]
fn foreach_over_every_iterable() {
    assert_eq!(int(eval("local s = 0; foreach (i, v in [5, 6, 7]) s += i * v; return s")), 20);
    assert_eq!(int(eval("local s = 0; foreach (c in \"AB\") s += c; return s")), 131);
    assert_eq!(int(eval("local s = 0; foreach (k, v in { a = 1, b = 2 }) s += v; return s")), 3);
    assert_eq!(
        int(eval("class C { a = 1; b = 2 } local n = 0; foreach (k, v in C) n++; return n")),
        2
    );
    assert_eq!(raised("foreach (v in 5) {}"), "cannot iterate integer");
}

#[test]
fn const_and_enum_are_compile_time() {
    assert_eq!(int(eval("const X = 5\nreturn X * 2")), 10);
    assert_eq!(int(eval("enum E { A, B = 7, C }\nreturn E.B + E.C")), 8);
    assert_eq!(compile_error("const X = 1\nX = 2"), "free variables cannot be modified");
}

#[test]
fn string_and_array_default_delegates() {
    assert_eq!(int(eval("return \"hello world\".find(\"o\", 5)")), 7);
    assert!(eval("return \"abc\".find(\"z\")").is_null());
    assert_eq!(string(eval("return \"abcdef\".slice(1, -1)")), "bcde");
    assert_eq!(int(eval("return \"42abc\".tointeger()")), 42);
    assert_eq!(string(eval("return \"MiX\".tolower() + \"MiX\".toupper()")), "mixMIX");
    assert_eq!(
        string(eval("local a = [3, 1, 2]; a.sort(); a.append(9); a.reverse(); return a[0] + \",\" + a.top() + \",\" + a.len()")),
        "9,1,4"
    );
    assert_eq!(
        int(eval("local a = [1,2,3]; a.sort(function(x, y) { return y - x }); return a[0]")),
        3
    );
    assert_eq!(raised("[].pop()"), "empty array");
}

#[test]
fn closures_bindenv_call_and_acall() {
    assert_eq!(
        int(eval("local t = { v = 3 }\nlocal f = function() { return v }.bindenv(t); return f()")),
        3
    );
    assert_eq!(int(eval("local f = function(a, b) { return a + b }\nreturn f.acall([null, 1, 2])")), 3);
    assert_eq!(int(eval("local f = function(a) { return this.k + a }\nreturn f.call({ k = 1 }, 2)")), 3);
}

#[test]
fn the_standard_library_valve_registers() {
    assert_eq!(string(eval("return format(\"%d-%05.1f-%s-%x\", 7, 3.14159, \"s\", 255)")), "7-003.1-s-ff");
    assert_eq!(string(eval("return strip(\"  a b  \")")), "a b");
    assert_eq!(int(eval("return split(\"a,,b;c\", \",;\").len()")), 3);
    assert_eq!(string(eval("return floor(2.7).tostring()")), "2");
    assert!(matches!(eval("local r = regexp(\"^On.*Output$\"); return r.match(\"OnFooOutput\")"), Value::Bool(true)));
    assert!(matches!(eval("local r = regexp(\"^On.*Output$\"); return r.match(\"OnFoo\")"), Value::Bool(false)));
    assert_eq!(int(eval("local r = regexp(\"[0-9]+\"); return r.search(\"ab123c\").begin")), 2);
}

#[test]
fn valves_vector_class() {
    assert_eq!(
        string(eval("local v = Vector(1, 2, 3) + Vector(1, 1, 1); return v.x + \",\" + v.y + \",\" + v.z")),
        "2,3,4"
    );
    assert_eq!(string(eval("return typeof Vector()")), "Vector");
    assert_eq!(
        string(eval("return Vector(1, 0, 0).tostring()")),
        "(vector : (1.000000, 0.000000, 0.000000))"
    );
    assert_eq!(int(eval("local v = Vector(3, 4, 0); v.z = 12; return v.Length().tointeger()")), 13);
}

#[test]
fn init_nut_defines_printl_and_the_scope_helpers() {
    assert_eq!(output("printl(\"hi\")"), "hi\n");
    let src = "
        local s = VSquirrel_OnCreateScope(\"myscope\", getroottable())
        s.q <- 1
        return (\"myscope\" in getroottable()) && s.q == 1 && s.printl == printl
    ";
    assert!(matches!(eval(src), Value::Bool(true)));
}

#[test]
fn an_uncaught_error_prints_the_callstack_and_a_caught_one_does_not() {
    let printed = output("function f() { local a = 1; return nothing } f()");
    assert!(printed.contains("AN ERROR HAS OCCURED [the index 'nothing' does not exist]"), "{printed}");
    assert!(printed.contains("*FUNCTION [f()] test line [1]"), "{printed}");
    assert!(printed.contains("[a] 1"), "{printed}");
    assert_eq!(output("try { nothing } catch (e) {}"), "");
}

#[test]
fn compile_errors_are_squirrels() {
    assert_eq!(compile_error("local = 1"), "expected 'IDENTIFIER'");
    assert_eq!(compile_error("x <- "), "expression expected");
    assert_eq!(compile_error("local x; x <- 1"), "can't 'create' a local slot");
    assert_eq!(compile_error("break"), "'break' has to be in a loop block");
    assert_eq!(compile_error("a b"), "end of statement expected (; or lf)");
    assert_eq!(compile_error("1e5"), "invalid numeric format");
}

/// Every script the game ships compiles. The depot has 92, 64,067 lines,
/// under `scripts/vscripts/` — none is in a VPK.
#[test]
#[ignore = "needs KISAK_GAME_DIR pointing at the Portal 2 mod directory"]
fn every_shipped_script_compiles() {
    let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
        return;
    };
    let root = std::path::Path::new(&dir).join("scripts/vscripts");
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("readable") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "nut") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut failures = Vec::new();
    let mut lines = 0;
    for path in &files {
        let source = std::fs::read(path).expect("readable");
        lines += source.iter().filter(|&&c| c == b'\n').count();
        let (mut vm, _) = vm();
        let name = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        if let Err(e) = vm.compile(&source, &name) {
            failures.push(format!("{name}:{}:{} {}", e.line, e.column, e.message));
        }
    }
    eprintln!("{} scripts, {} lines, {} failed", files.len(), lines, failures.len());
    assert_eq!(files.len(), 92);
    assert!(failures.is_empty(), "{failures:#?}");
}


#[test]
fn a_vector_answers_x_y_z_and_nothing_else() {
    // `_get` fails quietly for any other key, so the lookup falls through to
    // the instance's default delegate and then to an index error.
    assert_eq!(raised("return Vector(1, 2, 3).w"), "the index 'w' does not exist");
    assert_eq!(string(eval("return Vector(1, 2, 3).tostring()")), "(vector : (1.000000, 2.000000, 3.000000))");
    assert_eq!(string(eval("local v = Vector(1, 2, 3); v.Norm(); return format(\"%.3f\", v.Length())")), "1.000");
}

/// `rustdocs/VSCRIPT.md`'s quick start, verbatim.
#[test]
fn the_quick_start_in_the_rustdoc_runs() {
    let mut vm = Vm::new(0, Box::new(|text| eprint!("{text}")));
    let main = vm
        .compile(b"function Double(x) { return x * 2 }", "example.nut")
        .expect("compiles");
    vm.execute(&mut (), &main, None, &[]).expect("runs");

    let root = Value::Table(vm.root());
    let double = vm.get(&mut (), &root, &Value::str("Double")).expect("defined");
    let four = vm.call(&mut (), &double, root, &[Value::Integer(2)], true);
    assert!(matches!(four, Ok(Value::Integer(4))));

    let root = vm.root();
    vm.register_native(&root, "Twice", 2, ".n", |_, _, args| {
        Ok(Value::Integer(args[1].to_integer() * 2))
    });
    let f = vm.compile(b"return Twice(21)", "t").expect("compiles");
    assert!(matches!(vm.execute(&mut (), &f, None, &[]), Ok(Value::Integer(42))));
}
