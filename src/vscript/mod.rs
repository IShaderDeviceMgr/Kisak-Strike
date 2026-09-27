//! VScript: Squirrel 2.2, the language Portal 2's maps are scripted in.
//!
//! `legacy/vscript/languages/squirrel/` (Squirrel 2.2.3, 10,531 lines) plus the
//! half of `vsquirrel/vsquirrel.cpp` that is the language rather than the
//! engine binding: `CSquirrelVM::Init`'s standard libraries, Valve's `Vector`
//! class, and `init.nut`. The *game's* bindings — `EntFire`, `Entities`,
//! `self` — are `server::script`'s, exactly as `vscript_server.cpp` is the
//! server's and not the VM's.
//!
//! Status and shape are in `rustdocs/VSCRIPT.md`; the plan and the
//! measurements that scoped it are in `portdocs/VSCRIPT.md`. In one
//! paragraph: **the compiler's decisions are Squirrel's and the executor is a
//! tree walker**. Every name is resolved where `SQCompiler` resolves it,
//! every value has Squirrel's representation (32-bit integers, 32-bit floats,
//! byte strings, Lua-4 hash tables whose layout is the iteration order), and
//! every runtime rule — what `this` a call gets, when a lookup falls back to
//! the root table, which errors are caught and which reach the error handler
//! — follows `sqvm.cpp`. What is not here is `yield`/`resume` and threads,
//! which no shipped script uses, and the remote debugger.

mod ast;
mod baselib;
mod format;
mod interp;
mod lexer;
mod parser;
mod stdlib;
mod table;
mod value;
mod vector;

#[cfg(test)]
mod tests;

use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Instant;

pub use lexer::CompileError;
pub use value::{
    rt, ArrayRef, Class, ClassRef, Closure, Instance, InstanceRef, Native, SqStr,
    TableObj, TableRef, Value,
};
pub use vector::{vector_of, vector_value};

/// `SQMetaMethod` (`sqobject.h:14`), in order: the index is what a class's
/// `_metamethods` array is indexed by.
pub(crate) const METAMETHODS: [&str; 18] = [
    "_add", "_sub", "_mul", "_div", "_unm", "_modulo", "_set", "_get", "_typeof", "_nexti", "_cmp",
    "_call", "_cloned", "_newslot", "_delslot", "_tostring", "_newmember", "_inherited",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MetaMethod {
    Add = 0,
    Sub,
    Mul,
    Div,
    Unm,
    Modulo,
    Set,
    Get,
    Typeof,
    Nexti,
    Cmp,
    Call,
    Cloned,
    NewSlot,
    DelSlot,
    ToString,
    NewMember,
    Inherited,
}

/// The default delegates — the methods every value of a type answers to
/// (`SQSharedState::_table_default_delegate` and its siblings).
pub(crate) struct Delegates {
    pub table: TableRef,
    pub array: TableRef,
    pub string: TableRef,
    pub number: TableRef,
    pub closure: TableRef,
    pub class: TableRef,
    pub instance: TableRef,
    pub weakref: TableRef,
}

/// One call on the script stack: a closure's locals, or a native's name.
pub(crate) struct Frame {
    pub closure: Option<Rc<Closure>>,
    pub native_name: Option<SqStr>,
    pub locals: Vec<Value>,
    pub vargs: Vec<Value>,
    pub line: u32,
    /// The locals in scope, for the error handler's `LOCALS` dump.
    pub live: Vec<(SqStr, u16)>,
}

/// One `SQVM::Execute` — the unit of "is there a `try` around this". A native
/// that calls back into script starts a new one, which is why an error inside
/// an `array.sort` comparator reaches the error handler even when the sort is
/// itself inside a `try`.
pub(crate) struct Execute {
    pub traps: u32,
    pub raise_error: bool,
}

/// `SQ_QUERY_COUNT_START` (`sqvm.h:28`): jumps between checks of how long a
/// script has been running.
const QUERY_COUNT_START: u32 = 100_000;

/// `CSquirrelVM::QueryContinue`'s limit: a script that has been running this
/// long at a check is stopped.
const QUERY_TIME_LIMIT: f32 = 0.03;

/// `MAX_NATIVE_CALLS` (`sqvm.h:7`).
const MAX_NATIVE_CALLS: u32 = 100;

/// How deep script calls may nest. **Not Squirrel's** — its call stack is a
/// growable vector and has no limit — but a tree walker recurses on the host
/// stack, and a script that recursed ten thousand deep would take the process
/// with it rather than raise an error. No shipped script recurses more than a
/// few levels.
const MAX_SCRIPT_DEPTH: usize = 200;

/// A Squirrel VM: `sq_open` plus `CSquirrelVM::Init`.
pub struct Vm {
    pub(crate) root: TableRef,
    pub(crate) consts: TableRef,
    pub(crate) delegates: Delegates,
    pub(crate) error_handler: Value,
    /// `_lasterror` — kept across errors, because Valve's `ARITH_OP` appends
    /// it to the next arithmetic error's message.
    pub(crate) last_error: Value,
    pub(crate) frames: Vec<Frame>,
    pub(crate) executes: Vec<Execute>,
    pub(crate) native_calls: u32,
    print: Box<dyn FnMut(&str)>,
    /// Every container this VM made, so that dropping it can break the cycles
    /// `Rc` cannot — `sq_close` finalising the GC chain.
    objects: RefCell<Vec<Weak<dyn Finalize>>>,
    objects_pruned_at: std::cell::Cell<usize>,
    query_count: u32,
    execute_started: Option<Instant>,
    /// `developer`'s value, which `init.nut` branches on.
    developer: i32,
    /// `CSquirrelVM::m_hClassVector`.
    pub(crate) vector_class: Option<ClassRef>,
}

/// What `Vm`'s teardown does to one container.
pub(crate) trait Finalize {
    fn finalize(&self);
}

impl Finalize for RefCell<TableObj> {
    fn finalize(&self) {
        if let Ok(mut t) = self.try_borrow_mut() {
            t.table.finalize();
            t.delegate = None;
        }
    }
}

impl Finalize for RefCell<Vec<Value>> {
    fn finalize(&self) {
        if let Ok(mut a) = self.try_borrow_mut() {
            a.clear();
        }
    }
}

impl Finalize for RefCell<Class> {
    fn finalize(&self) {
        if let Ok(mut c) = self.try_borrow_mut() {
            c.members.finalize();
            c.default_values.clear();
            c.methods.clear();
            c.metamethods.iter_mut().for_each(|m| *m = Value::Null);
            c.attributes = Value::Null;
            c.base = None;
        }
    }
}

impl Finalize for RefCell<Instance> {
    fn finalize(&self) {
        if let Ok(mut i) = self.try_borrow_mut() {
            i.values.clear();
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.frames.clear();
        for object in self.objects.borrow().iter() {
            if let Some(object) = object.upgrade() {
                object.finalize();
            }
        }
    }
}

impl Vm {
    /// `sq_open` — the base library and nothing else. What a test of the
    /// language alone wants.
    pub fn bare() -> Vm {
        let empty = || Rc::new(RefCell::new(TableObj::new(0)));
        let mut vm = Vm {
            root: empty(),
            consts: empty(),
            delegates: Delegates {
                table: empty(),
                array: empty(),
                string: empty(),
                number: empty(),
                closure: empty(),
                class: empty(),
                instance: empty(),
                weakref: empty(),
            },
            error_handler: Value::Null,
            last_error: Value::Null,
            frames: Vec::new(),
            executes: Vec::new(),
            native_calls: 0,
            print: Box::new(|text| eprint!("{text}")),
            objects: RefCell::new(Vec::new()),
            objects_pruned_at: std::cell::Cell::new(0),
            query_count: QUERY_COUNT_START,
            execute_started: None,
            developer: 0,
            vector_class: None,
        };
        vm.track_table(&vm.root.clone());
        baselib::register(&mut vm);
        vm
    }

    /// `CSquirrelVM::Init` (`vsquirrel.cpp:570`): the base library, the math
    /// and string libraries, the standard error handler, `developer()`,
    /// Valve's `Vector`, and `init.nut`.
    pub fn new(developer: i32, print: Box<dyn FnMut(&str)>) -> Vm {
        let mut vm = Vm::bare();
        vm.print = print;
        vm.developer = developer;
        stdlib::register_math(&mut vm);
        stdlib::register_string(&mut vm);
        stdlib::set_error_handlers(&mut vm);
        let root = vm.root.clone();
        vm.register_native(&root, "developer", 1, "", |vm, _, _| {
            Ok(Value::Integer(vm.developer))
        });
        vm.register_native(&root, "GetFunctionSignature", 0, "", baselib::get_function_signature);
        vector::register(&mut vm);
        let init = vm
            .compile(INIT_NUT, "init.nut")
            .expect("init.nut is Valve's and compiles");
        if let Err(err) = vm.execute(&mut (), &init, None, &[]) {
            vm.print(&format!("init.nut failed: {err:?}\n"));
        }
        vm
    }

    pub fn print(&mut self, text: &str) {
        (self.print)(text);
    }

    pub fn developer(&self) -> i32 {
        self.developer
    }

    pub fn root(&self) -> TableRef {
        self.root.clone()
    }

    pub fn consts(&self) -> TableRef {
        self.consts.clone()
    }

    /// `sq_compilebuffer` — a closure for the file's main function, or the
    /// compiler's error, which is also printed the way
    /// `_sqstd_compiler_error` prints it.
    pub fn compile(&mut self, source: &[u8], name: &str) -> Result<Value, CompileError> {
        match parser::compile(source, name, &self.consts) {
            Ok(proto) => Ok(Value::Closure(Rc::new(Closure {
                proto,
                outers: Vec::new(),
                defaults: Vec::new(),
                env: None,
            }))),
            Err(err) => {
                self.print(&format!(
                    "{} line = ({}) column = ({}) : error {}\n",
                    name, err.line, err.column, err.message
                ));
                Err(err)
            }
        }
    }

    /// `CSquirrelVM::ExecuteFunction` — calls `function` with `this` set to
    /// `scope` (or the root table) and reports an uncaught error through the
    /// error handler. Starts the "running too long" clock.
    pub fn execute(
        &mut self,
        host: &mut dyn Any,
        function: &Value,
        scope: Option<&Value>,
        args: &[Value],
    ) -> Result<Value, Value> {
        let this = match scope {
            Some(scope) => scope.clone(),
            None => Value::Table(self.root.clone()),
        };
        let outermost = self.execute_started.is_none();
        if outermost {
            self.execute_started = Some(Instant::now());
        }
        let result = self.call(host, function, this, args, true);
        if outermost {
            self.execute_started = None;
        }
        result
    }

    /// `sq_call` — calls any callable from native code, as a new execution
    /// with its own `try` accounting. `raise_error` is whether an error that
    /// nothing catches goes to the error handler.
    pub fn call(
        &mut self,
        host: &mut dyn Any,
        function: &Value,
        this: Value,
        args: &[Value],
        raise_error: bool,
    ) -> Result<Value, Value> {
        self.call_from_native(host, function, this, args, raise_error)
    }

    /// A new, empty table, tracked for teardown.
    pub fn new_table(&self, initial_size: usize) -> TableRef {
        let table = Rc::new(RefCell::new(TableObj::new(initial_size)));
        self.track_table(&table);
        table
    }

    pub fn new_array(&self, values: Vec<Value>) -> ArrayRef {
        let array = Rc::new(RefCell::new(values));
        self.track(Rc::downgrade(&array) as Weak<dyn Finalize>);
        array
    }

    pub(crate) fn track_table(&self, table: &TableRef) {
        self.track(Rc::downgrade(table) as Weak<dyn Finalize>);
    }

    pub(crate) fn track(&self, object: Weak<dyn Finalize>) {
        let mut objects = self.objects.borrow_mut();
        objects.push(object);
        // Drop the dead ones now and then, so that a script making
        // temporaries in a loop does not grow the list without bound.
        if objects.len() > 1024 && objects.len() > 2 * self.objects_pruned_at.get() {
            objects.retain(|o| o.strong_count() > 0);
            self.objects_pruned_at.set(objects.len());
        }
    }

    /// Adds a native function to `table` — `sq_newclosure` +
    /// `sq_setparamscheck` + `sq_createslot`. `typemask` is Squirrel's
    /// parameter type string (`".sn"`), and `nparams` counts `this`.
    pub fn register_native(
        &mut self,
        table: &TableRef,
        name: &str,
        nparams: i32,
        typemask: &str,
        func: impl Fn(&mut Vm, &mut dyn Any, &[Value]) -> Result<Value, Value> + 'static,
    ) {
        let native = self.native(name, nparams, typemask, func);
        table
            .borrow_mut()
            .table
            .new_slot(Value::str(name), native);
    }

    /// A native closure value.
    pub fn native(
        &self,
        name: &str,
        nparams: i32,
        typemask: &str,
        func: impl Fn(&mut Vm, &mut dyn Any, &[Value]) -> Result<Value, Value> + 'static,
    ) -> Value {
        Value::Native(Rc::new(Native {
            name: SqStr::from_str(name),
            func: Rc::new(func),
            nparamscheck: nparams,
            typecheck: compile_typemask(typemask),
            env: None,
        }))
    }

    /// A new class — `sq_newclass`. Its methods are added with
    /// [`Vm::class_new_slot`].
    pub fn new_class(&self, base: Option<ClassRef>, type_tag: usize) -> ClassRef {
        let class = interp::create_class(base);
        class.borrow_mut().type_tag = type_tag;
        self.track(Rc::downgrade(&class) as Weak<dyn Finalize>);
        class
    }

    /// A method or field on a class, as `class.key <- value` would add it.
    pub fn class_new_slot(&self, class: &ClassRef, key: &str, value: Value) {
        interp::class_new_slot(class, Value::str(key), value, false);
    }

    /// An instance of `class` with native data hung off it, and no
    /// constructor run — `sq_createinstance` + `sq_setinstanceup`.
    pub fn new_instance(&self, class: &ClassRef, user: Option<Box<dyn Any>>) -> InstanceRef {
        let instance = interp::create_instance(class);
        instance.borrow_mut().user = user;
        self.track(Rc::downgrade(&instance) as Weak<dyn Finalize>);
        instance
    }

    /// `table[key]` with delegates and metamethods, as a script would read it
    /// — `sq_get`.
    pub fn get(&mut self, host: &mut dyn Any, object: &Value, key: &Value) -> Option<Value> {
        self.get_value(host, object, key, false, false)
    }

    /// `object[key] <- value`, with `_newslot` and the class rules —
    /// `sq_newslot`, which is what Valve's `SetValue` calls.
    pub fn new_slot_value(
        &mut self,
        host: &mut dyn Any,
        object: &Value,
        key: Value,
        value: Value,
    ) -> Result<(), Value> {
        self.new_slot(host, object, key, value, false)
    }

    /// `delete object[key]` — `sq_deleteslot`, which is what Valve's
    /// `ClearValue` calls.
    pub fn delete_slot_value(&mut self, host: &mut dyn Any, object: &Value, key: &Value) -> Result<Value, Value> {
        self.delete_slot(host, object, key)
    }

    /// `table[key] <- value` on a table, raw.
    pub fn set_slot(&self, table: &TableRef, key: &str, value: Value) {
        table.borrow_mut().table.new_slot(Value::str(key), value);
    }

    /// Clears the "running too long" clock, for a caller that knows a new
    /// top-level execution starts now.
    pub(crate) fn tick_query(&mut self) -> Result<(), Value> {
        self.query_count = self.query_count.saturating_sub(1);
        if self.query_count > 0 {
            return Ok(());
        }
        self.query_count = QUERY_COUNT_START;
        if let Some(started) = self.execute_started {
            if started.elapsed().as_secs_f32() > QUERY_TIME_LIMIT {
                self.print("Script running too long, terminating\n");
                return Err(Value::str("Script terminated by SQQuerySuspend"));
            }
        }
        Ok(())
    }
}

/// `sq_setparamscheck`'s type string, compiled (`sqapi.cpp`'s
/// `CompileTypemask`): one mask per parameter, `|` joining alternatives and
/// spaces ignored.
fn compile_typemask(typemask: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut mask = 0u32;
    let mut pending = false;
    for c in typemask.bytes() {
        let bits = match c {
            b'o' => rt::NULL,
            b'i' => rt::INTEGER,
            b'f' => rt::FLOAT,
            b'n' => rt::FLOAT | rt::INTEGER,
            b's' => rt::STRING,
            b't' => rt::TABLE,
            b'a' => rt::ARRAY,
            b'u' => rt::USERDATA,
            b'c' => rt::CLOSURE | rt::NATIVECLOSURE,
            b'b' => rt::BOOL,
            b'g' => rt::GENERATOR,
            b'p' => rt::USERPOINTER,
            b'v' => rt::THREAD,
            b'x' => rt::INSTANCE,
            b'y' => rt::CLASS,
            b'r' => rt::WEAKREF,
            b'.' => u32::MAX,
            b' ' => continue,
            b'|' => {
                pending = true;
                continue;
            }
            _ => continue,
        };
        if pending {
            mask |= bits;
            pending = false;
            if let Some(last) = out.last_mut() {
                *last = mask;
            }
            continue;
        }
        mask = bits;
        out.push(mask);
    }
    out
}

/// `vsquirrel/init.nut` — Valve's first script, run in every VM. It is the
/// C++ tree's own file, byte for byte; it is Latin-1 (the copyright line) and
/// so read as bytes.
const INIT_NUT: &[u8] = include_bytes!("init.nut");
