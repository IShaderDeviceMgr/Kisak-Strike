//! The server's half of VScript: `vscript_server.cpp`, `vscript_shared.cpp`,
//! and the script half of `CBaseEntity` (`baseentity.cpp:7373-8660`).
//!
//! **One VM per level**, made at `LevelInitPreEntity` and dropped at
//! `LevelShutdownPostEntity`, exactly as `CVScriptGameSystem` does — so a
//! script's globals (`::TransitionFired`) never survive a map change, and the
//! maps rely on that.
//!
//! **An entity's scope is a table in the root table**, keyed by its script id
//! and delegating to the root (`VSquirrel_OnCreateScope` in `init.nut`), and
//! `self` in it is the entity's *instance* — a `CBaseEntity` class instance
//! whose natives reach back into the entity list. A script reaches an
//! unqualified global because the scope's delegate is the root table; it
//! reaches the entity through `self`.
//!
//! **The VM is lifted out of the server while it runs** ([`Server::with_vm`]),
//! and the natives are handed the server as their host — so a native can read
//! and write the entity list, queue events and look up instances, and the
//! borrow checker has nothing to object to. What a native cannot do is run
//! *another* script except through the VM it was handed, which is also the
//! only way Valve's could.
//!
//! What a native needs that the port has not got is **not registered**, so a
//! script that calls it fails with Squirrel's own "the index 'X' does not
//! exist" rather than getting a stub that pretends. The biggest absence by far
//! is `CreateSceneEntity` — 4,647 of the calls in the shipped scripts, all of
//! them GLaDOS's choreography — and `rustdocs/VSCRIPT.md` has the full list.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glam::Vec3;

use super::entity::EntityId;
use super::movement::FL_ONGROUND;
use super::io::{Event, EventAction, Target, Variant, EVENT_FIRE_ALWAYS};
use super::name::{self, Procedural};
use super::touch::Teleport;
use super::{attachment, hierarchy, keyvalue, Server};
use crate::vscript::{self, ClassRef, InstanceRef, TableRef, Value, Vm};

/// Where `scripts/vscripts/*.nut` come from — the engine's filesystem, or a
/// test's table. `filesystem->ReadFile( scriptPath, "GAME", … )`.
pub trait ScriptFiles {
    fn read_script(&self, path: &str) -> Option<Vec<u8>>;
}

/// `sv_script_think_interval` (`baseentity.cpp:109`).
const SCRIPT_THINK_INTERVAL: f32 = 0.1;

/// `g_ScriptServerRunScriptDepth`'s limit (`vscript_shared.cpp`).
const MAX_RUN_DEPTH: u32 = 16;

/// How many printed lines [`ScriptState::output`] keeps.
const OUTPUT_LINES: usize = 4096;

/// `CBaseEntity::ScriptThink`'s context — one entity's `thinkfunction` and
/// when it next runs.
#[derive(Debug, Clone)]
pub(super) struct ScriptThink {
    pub entity: EntityId,
    pub function: String,
    /// The tick it next runs on. `SetContextThink` goes through
    /// `TIME_TO_TICKS` like every other think.
    pub tick: i32,
}

/// Everything about scripting that belongs to the level.
#[derive(Default)]
pub(super) struct ScriptState {
    /// `g_pScriptVM`. `None` between levels, **and while a script is running**
    /// — see [`Server::with_vm`].
    pub vm: Option<Vm>,
    /// Whether this level made a VM at all — what separates "a script is
    /// running" from "there is no scripting" when [`vm`](ScriptState::vm) is
    /// `None`.
    active: bool,
    /// The registered `CBaseEntity` script class.
    entity_class: Option<ClassRef>,
    /// `CBasePlayer`'s, which extends it — what the player's instance is.
    player_class: Option<ClassRef>,
    /// `m_hScriptInstance`, by entity.
    instances: HashMap<EntityId, InstanceRef>,
    /// `m_ScriptScope`, by entity.
    scopes: HashMap<EntityId, TableRef>,
    /// `m_iszScriptId`.
    script_ids: HashMap<EntityId, String>,
    /// `m_iUniqueIdSerialNumber`.
    serial: u64,
    pub thinks: Vec<ScriptThink>,
    /// `g_ScriptServerRunScriptDepth`.
    run_depth: u32,
    /// What scripts printed, a line at a time — the tail of it.
    pub output: Rc<RefCell<Vec<String>>>,
    /// Errors, compile failures and missing files, for the level summary.
    pub errors: usize,
}

fn server(host: &mut dyn Any) -> &mut Server {
    host.downcast_mut::<Server>()
        .expect("a server script native is always called with the server as its host")
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

fn string_arg(args: &[Value], i: usize) -> String {
    match args.get(i) {
        Some(Value::String(s)) => s.to_string_lossy(),
        _ => String::new(),
    }
}

fn vector_arg(args: &[Value], i: usize) -> Result<Vec3, Value> {
    args.get(i)
        .and_then(vscript::vector_of)
        .map(Vec3::from)
        .ok_or_else(|| Value::str("Vector argument expected"))
}

/// `ToEnt` — the entity a script handle names, if it still exists.
pub(super) fn to_ent(value: &Value) -> Option<EntityId> {
    let Value::Instance(i) = value else {
        return None;
    };
    let i = i.borrow();
    i.user.as_ref()?.downcast_ref::<EntityId>().copied()
}

/// `ScriptVariant_t::m_bool` read off whatever a script returned — which is
/// the union's low byte, so an integer 256 is `false` and a missing return
/// value is `false`. `AcceptInput` and `ScriptThink` read it this way.
fn variant_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Integer(i) => *i as u8 != 0,
        Value::Float(f) => f.to_bits() as u8 != 0,
        Value::Null => false,
        _ => true,
    }
}

impl ScriptState {
    fn print_sink(&self) -> Box<dyn FnMut(&str)> {
        let output = self.output.clone();
        let mut partial = String::new();
        Box::new(move |text: &str| {
            partial.push_str(text);
            while let Some(end) = partial.find('\n') {
                let line: String = partial.drain(..=end).collect();
                let line = line.trim_end_matches('\n').to_owned();
                eprintln!("source-engine: vscript: {line}");
                let mut out = output.borrow_mut();
                if out.len() >= OUTPUT_LINES {
                    out.remove(0);
                }
                out.push(line);
            }
        })
    }
}

impl Server {
    /// Sets where script files are read from. Called once by the engine; a
    /// test hands in a table.
    pub fn set_script_files(&mut self, files: Rc<dyn ScriptFiles>) {
        self.script_files = Some(files);
    }

    /// What the level's scripts have printed, most recent last — for a test,
    /// and for the day there is a console to show it in.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn script_output(&self) -> Vec<String> {
        self.script.output.borrow().clone()
    }

    /// Console commands scripts asked for — `SendToConsole` and
    /// `SendToConsoleServer` — for the engine to run.
    pub fn take_console_commands(&mut self) -> Vec<String> {
        std::mem::take(&mut self.console_commands)
    }

    /// Runs `f` with the VM lifted out of the server, so that the natives it
    /// calls can be handed the whole server. `None` if there is no VM — or if
    /// one is already running, which is the one re-entry the port refuses
    /// and Valve's never needed.
    pub(super) fn with_vm<R>(&mut self, f: impl FnOnce(&mut Vm, &mut Server) -> R) -> Option<R> {
        let mut vm = self.script.vm.take()?;
        let result = f(&mut vm, self);
        self.script.vm = Some(vm);
        Some(result)
    }

    fn script_warning(&mut self, message: &str) {
        eprintln!("source-engine: vscript: {message}");
        self.script.errors += 1;
    }

    // ----- lifetime ---------------------------------------------------------

    /// `VScriptServerInit` (`vscript_server.cpp:760`) — at
    /// `LevelInitPreEntity`, before any entity exists.
    pub(super) fn script_init(&mut self) {
        self.script_shutdown();
        let output = self.script.output.clone();
        output.borrow_mut().clear();
        let mut vm = Vm::new(0, self.script.print_sink());
        self.script.active = true;
        register_globals(&mut vm);
        let entity_class = register_entity_class(&mut vm);
        self.script.player_class = Some(register_player_class(&mut vm, &entity_class));
        self.script.entity_class = Some(entity_class);
        register_entities_singleton(&mut vm);
        match vm.compile(VSCRIPT_SERVER_NUT, "vscript_server.nut") {
            Ok(f) => {
                if vm.execute(self, &f, None, &[]).is_err() {
                    self.script_warning("vscript_server.nut raised an error");
                }
            }
            Err(_) => self.script_warning("vscript_server.nut failed to compile"),
        }
        self.run_script_file(&mut vm, "mapspawn", None, false);
        self.script.vm = Some(vm);
    }

    /// `VScriptServerTerm` — the VM and every handle into it go.
    pub(super) fn script_shutdown(&mut self) {
        let output = self.script.output.clone();
        self.script = ScriptState {
            output,
            ..ScriptState::default()
        };
    }

    // ----- instances and scopes ----------------------------------------------

    /// `CBaseEntity::GetScriptInstance` (`baseentity.cpp:8491`).
    fn script_instance(&mut self, vm: &mut Vm, id: EntityId) -> Value {
        if let Some(instance) = self.script.instances.get(&id) {
            return Value::Instance(instance.clone());
        }
        if !self.entities.is_alive(id) {
            return Value::Null;
        }
        if !self.script.script_ids.contains_key(&id) {
            let root = self
                .entities
                .get(id)
                .map(|e| e.name.clone().unwrap_or_else(|| e.classname().to_owned()))
                .unwrap_or_default();
            let key = self.generate_unique_key(&root);
            self.script.script_ids.insert(id, key);
        }
        // `GetScriptDesc()` — the most derived class with a script
        // description. Only the player's has members beyond `CBaseEntity`'s
        // here; `CBaseAnimating`'s and the rest are not registered yet.
        let class = match self.player == Some(id) {
            true => self.script.player_class.clone(),
            false => self.script.entity_class.clone(),
        }
        .expect("the entity classes are registered at init");
        let instance = vm.new_instance(&class, Some(Box::new(id)));
        self.script.instances.insert(id, instance.clone());
        Value::Instance(instance)
    }

    /// `CSquirrelVM::GenerateUniqueKey` — `"%x%x%llx_%s"` of a random number,
    /// `Plat_MSTime()` and a serial. The random number comes from the level's
    /// stream, which is Valve's (`RandomInt` is the global uniform stream);
    /// the milliseconds are the **level** clock's rather than the process's,
    /// so that the key — which a script can see — is reproducible.
    fn generate_unique_key(&mut self, root: &str) -> String {
        let random = self.random.int(0, 0xfff);
        let ms = (self.clock.time().curtime * 1000.0) as u32;
        let serial = self.script.serial;
        self.script.serial += 1;
        format!("{random:x}{ms:x}{serial:x}_{root}")
    }

    /// `CBasePlayer::Spawn`'s `SetValue( "player", GetScriptInstance() )`
    /// (`player.cpp:5281`) — in a single-player game the root table's `player`
    /// is the player, from the moment it spawns.
    pub(super) fn script_player_spawned(&mut self, id: EntityId) {
        self.with_vm(|vm, server| {
            let instance = server.script_instance(vm, id);
            let root = Value::Table(vm.root());
            let _ = vm.new_slot_value(server, &root, Value::str("player"), instance);
        });
    }

    /// `CBaseEntity::ValidateScriptScope` (`baseentity.cpp:8512`).
    fn validate_script_scope(&mut self, vm: &mut Vm, id: EntityId) -> Option<TableRef> {
        if let Some(scope) = self.script.scopes.get(&id) {
            return Some(scope.clone());
        }
        let instance = self.script_instance(vm, id);
        if instance.is_null() {
            return None;
        }
        let key = self.script.script_ids.get(&id)?.clone();
        // `CSquirrelVM::CreateScope` → `VSquirrel_OnCreateScope( name, root )`.
        let root = Value::Table(vm.root());
        let create = vm.get(self, &root, &Value::str("VSquirrel_OnCreateScope"))?;
        let scope = match vm.call(self, &create, root.clone(), &[Value::str(&key), root], true) {
            Ok(Value::Table(t)) => t,
            _ => {
                let name = self.debug_name(id);
                self.script_warning(&format!("{name} couldn't create ScriptScope!"));
                return None;
            }
        };
        let _ = vm.new_slot_value(self, &Value::Table(scope.clone()), Value::str("self"), instance);
        self.script.scopes.insert(id, scope.clone());
        Some(scope)
    }

    fn debug_name(&self, id: EntityId) -> String {
        self.entities
            .get(id)
            .map_or_else(|| "<removed>".to_owned(), |e| e.debug_name().to_owned())
    }

    /// Whether an entity has a scope — `m_ScriptScope.IsInitialized()`.
    pub(super) fn has_script_scope(&self, id: EntityId) -> bool {
        self.script.scopes.contains_key(&id)
    }

    /// The instances and scopes of entities that no longer exist —
    /// `UpdateOnRemove`'s `RemoveInstance` and the scope's `Term`, which
    /// calls `VSquirrel_OnReleaseScope` and takes the scope out of the root
    /// table.
    pub(super) fn script_forget_dead(&mut self) {
        if !self.script.active {
            return;
        }
        let dead: Vec<EntityId> = self
            .script
            .instances
            .keys()
            .chain(self.script.scopes.keys())
            .copied()
            .filter(|&id| !self.entities.is_alive(id))
            .collect();
        if dead.is_empty() {
            return;
        }
        self.script.thinks.retain(|t| dead.iter().all(|&d| d != t.entity));
        // `CBasePlayer::UpdateOnRemove`: `SetValue( "player", null )`.
        let player_died = dead.iter().any(|id| {
            self.script
                .instances
                .get(id)
                .is_some_and(|i| i.borrow().class.borrow().type_tag == PLAYER_TYPE_TAG)
        });
        let mut scopes = Vec::new();
        for id in &dead {
            if let Some(instance) = self.script.instances.remove(id) {
                instance.borrow_mut().user = None;
            }
            if let Some(scope) = self.script.scopes.remove(id) {
                scopes.push(scope);
            }
            self.script.script_ids.remove(id);
        }
        self.with_vm(|vm, server| {
            let root = Value::Table(vm.root());
            if player_died {
                let _ = vm.new_slot_value(server, &root, Value::str("player"), Value::Null);
            }
            let Some(release) = vm.get(server, &root, &Value::str("VSquirrel_OnReleaseScope")) else {
                return;
            };
            for scope in scopes {
                let _ = vm.call(server, &release, root.clone(), &[Value::Table(scope)], true);
            }
        });
    }

    // ----- running scripts ---------------------------------------------------

    /// `VScriptRunScript` (`vscript_shared.cpp`) — `scripts/vscripts/<name>`
    /// (`.nut` added if it has no extension), run in `scope` or the root.
    pub(super) fn run_script_file(
        &mut self,
        vm: &mut Vm,
        name: &str,
        scope: Option<Value>,
        warn_missing: bool,
    ) -> bool {
        if name.is_empty() {
            self.script_warning("Cannot run script: NULL script name");
            return false;
        }
        if self.script.run_depth > MAX_RUN_DEPTH {
            self.script_warning("IncludeScript stack overflow");
            return false;
        }
        let extension = name.rfind('.').map(|i| &name[i..]);
        if extension.is_some_and(|e| e != ".nut") {
            self.script_warning("Script file type does not match VM type");
            return false;
        }
        let path = match extension {
            Some(_) => format!("scripts/vscripts/{name}"),
            None => format!("scripts/vscripts/{name}.nut"),
        };
        let source = self.script_files.as_ref().and_then(|files| files.read_script(&path));
        let Some(source) = source.filter(|s| !s.is_empty() && s[0] != 0) else {
            if warn_missing {
                self.script_warning(&format!("Script not found ({path})"));
            }
            return false;
        };
        let file_name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let Ok(function) = vm.compile(&source, &file_name) else {
            self.script_warning(&format!("FAILED to compile and execute script file named {path}"));
            return false;
        };
        self.script.run_depth += 1;
        // "player" — `SetValue( "player", … )` before every run, in a
        // single-player game with a player.
        if let Some(player) = self.player {
            let instance = self.script_instance(vm, player);
            let root = Value::Table(vm.root());
            let _ = vm.new_slot_value(self, &root, Value::str("player"), instance);
        }
        let ok = vm.execute(self, &function, scope.as_ref(), &[]).is_ok();
        self.script.run_depth -= 1;
        if !ok {
            self.script_warning(&format!("Error running script named {name}"));
        }
        ok
    }

    /// `CBaseEntity::CallScriptFunction` (`baseentity.cpp:7445`) — `Some` of
    /// what it returned if the scope has a *script* function by that name,
    /// `None` if it has not.
    fn call_script_function(&mut self, vm: &mut Vm, id: EntityId, function: &str) -> Option<Value> {
        let scope = self.validate_script_scope(vm, id)?;
        let scope = Value::Table(scope);
        let f = vm.get(self, &scope, &Value::str(function))?;
        if !matches!(f, Value::Closure(_)) {
            return None;
        }
        let root = Value::Table(vm.root());
        let instance = self.script_instance(vm, id);
        let _ = vm.new_slot_value(self, &root, Value::str("owninginstance"), instance);
        let result = vm.execute(self, &f, Some(&scope), &[]).unwrap_or_default();
        let _ = vm.delete_slot_value(self, &root, &Value::str("owninginstance"));
        Some(result)
    }

    /// `CBaseEntity::RunScript` (`baseentity.cpp:7632`) — `RunScriptCode`'s
    /// body: compile the text as `InputRunScript` and run it in the scope.
    fn run_script_code(&mut self, vm: &mut Vm, id: EntityId, code: &str) {
        let Some(scope) = self.validate_script_scope(vm, id) else {
            return;
        };
        if code.is_empty() {
            return;
        }
        let ok = match vm.compile(code.as_bytes(), "InputRunScript") {
            Ok(f) => vm.execute(self, &f, Some(&Value::Table(scope)), &[]).is_ok(),
            Err(_) => false,
        };
        if !ok {
            let name = self.debug_name(id);
            self.script_warning(&format!(" Entity {name} encountered an error in RunScript()"));
        }
    }

    /// `CBaseEntity::RunScriptFile` (`baseentity.cpp:7610`).
    fn run_entity_script_file(&mut self, vm: &mut Vm, id: EntityId, file: &str, use_root: bool) -> bool {
        let Some(scope) = self.validate_script_scope(vm, id) else {
            return false;
        };
        match use_root {
            true => self.run_script_file(vm, file, None, true),
            false => self.run_script_file(vm, file, Some(Value::Table(scope)), true),
        }
    }

    /// `CBaseEntity::RunVScripts` + `RunPrecacheScripts` — called by
    /// `DispatchSpawn` **before** `Spawn`.
    pub(super) fn run_vscripts(&mut self, id: EntityId) {
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        let files = entity.core.vscripts.clone();
        let think = entity.core.script_think_function.clone();
        let is_world = entity.classname() == "worldspawn";
        let group = entity
            .behaviour
            .downcast_ref::<super::classes::LogicScript>()
            .map(|s| s.group.clone());
        if files.is_none() && group.is_none() {
            return;
        }
        self.with_vm(|vm, server| {
            // `CLogicScript::RunVScripts` (`logicentities.cpp:50`): the
            // `EntityGroup` array, then the base class.
            if let Some(group) = group {
                server.build_entity_group(vm, id, &group);
            }
            let Some(files) = files else {
                return;
            };
            if server.validate_script_scope(vm, id).is_none() {
                return;
            }
            let scope = Value::Table(server.script.scopes[&id].clone());
            for chain in ["OnPostSpawn", "Precache"] {
                let code = format!(
                    "{chain}CallChain <- CSimpleCallChainer(\"{chain}\", self.GetScriptScope(), true)"
                );
                if let Ok(f) = vm.compile(code.as_bytes(), "unnamed") {
                    let _ = vm.execute(server, &f, Some(&scope), &[]);
                }
            }
            // `char szScriptsList[255]` — the list is truncated to 254
            // characters before it is split.
            let list: String = files.chars().take(254).collect();
            for file in list.split(' ').filter(|f| !f.is_empty()) {
                server.run_entity_script_file(vm, id, file, is_world);
                for chain in ["OnPostSpawn", "Precache"] {
                    let code = format!("{chain}CallChain.PostScriptExecute()");
                    if let Ok(f) = vm.compile(code.as_bytes(), "unnamed") {
                        let _ = vm.execute(server, &f, Some(&scope), &[]);
                    }
                }
            }
            if let Some(function) = think {
                let time = server.clock.time();
                server.script.thinks.retain(|t| t.entity != id);
                server.script.thinks.push(ScriptThink {
                    entity: id,
                    function,
                    tick: time.time_to_ticks(time.curtime + SCRIPT_THINK_INTERVAL),
                });
            }
            // `RunPrecacheScripts`.
            if let Some(precache) = vm.get(server, &scope, &Value::str("DispatchPrecache")) {
                if matches!(precache, Value::Closure(_)) {
                    let _ = vm.execute(server, &precache, Some(&scope), &[]);
                }
            }
        });
    }

    /// `CLogicScript::RunVScripts`'s preamble — `__AppendToScriptGroup` over
    /// every `GroupNN` up to the last one set.
    fn build_entity_group(&mut self, vm: &mut Vm, id: EntityId, group: &[Option<String>]) {
        let Some(last) = group.iter().rposition(Option::is_some) else {
            return;
        };
        let Some(scope) = self.validate_script_scope(vm, id) else {
            return;
        };
        let scope = Value::Table(scope);
        let Ok(f) = vm.compile(ENTITY_GROUP_NUT, "unnamed") else {
            return;
        };
        let _ = vm.execute(self, &f, Some(&scope), &[]);
        let Some(append) = vm.get(self, &scope, &Value::str("__AppendToScriptGroup")) else {
            return;
        };
        for member in &group[..=last] {
            let name = Value::str(member.as_deref().unwrap_or(""));
            let _ = vm.execute(self, &append, Some(&scope), &[name]);
        }
        let _ = vm.delete_slot_value(self, &scope, &Value::str("__AppendToScriptGroup"));
    }

    /// `CBaseEntity::RunOnPostSpawnScripts` (`baseentity.cpp:8642`) — after
    /// `Spawn`. `ConnectOutputs` is commented out of the Portal 2 branch's
    /// `vscript_server.nut`, so what remains is the `DispatchOnPostSpawn`
    /// event every scripted entity posts to itself.
    pub(super) fn run_on_post_spawn_scripts(&mut self, id: EntityId) {
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.core.vscripts.is_none() || !self.has_script_scope(id) {
            return;
        }
        let scope = Value::Table(self.script.scopes[&id].clone());
        let found = self
            .with_vm(|vm, server| {
                let root = Value::Table(vm.root());
                if let Some(connect) = vm.get(server, &root, &Value::str("ConnectOutputs")) {
                    let _ = vm.execute(server, &connect, None, &[scope.clone()]);
                }
                vm.get(server, &scope, &Value::str("DispatchOnPostSpawn"))
                    .is_some_and(|f| matches!(f, Value::Closure(_)))
            })
            .unwrap_or(false);
        if found {
            let fire_time = self.clock.time().curtime;
            self.queue.add(Event {
                fire_time,
                target: Target::Entity(id),
                input: "CallScriptFunction".into(),
                value: Variant::String("DispatchOnPostSpawn".into()),
                activator: Some(id),
                caller: Some(id),
                output_id: 0,
            });
        }
    }

    // ----- inputs --------------------------------------------------------------

    /// The script half of `CBaseEntity::AcceptInput` (`baseentity.cpp:4536`):
    /// with a scope, `activator` and `caller` are set in the root table and
    /// `Input<name>` is called, and **its return value decides whether the
    /// input runs at all**. Returns whether it should.
    pub(super) fn script_input_hook(
        &mut self,
        id: EntityId,
        input: &str,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
    ) -> bool {
        if !self.has_script_scope(id) {
            return true;
        }
        self.with_vm(|vm, server| {
            let root = Value::Table(vm.root());
            let a = activator.map_or(Value::Null, |a| server.script_instance(vm, a));
            let c = caller.map_or(Value::Null, |c| server.script_instance(vm, c));
            let _ = vm.new_slot_value(server, &root, Value::str("activator"), a);
            let _ = vm.new_slot_value(server, &root, Value::str("caller"), c);
            match server.call_script_function(vm, id, &format!("Input{input}")) {
                Some(result) => variant_bool(&result),
                None => true,
            }
        })
        .unwrap_or(true)
    }

    /// `ClearValue( "activator" )`/`ClearValue( "caller" )`, after the input.
    pub(super) fn script_input_done(&mut self, id: EntityId) {
        if !self.has_script_scope(id) {
            return;
        }
        self.with_vm(|vm, server| {
            let root = Value::Table(vm.root());
            let _ = vm.delete_slot_value(server, &root, &Value::str("activator"));
            let _ = vm.delete_slot_value(server, &root, &Value::str("caller"));
        });
    }

    /// `InputRunScriptFile`, `InputRunScript` and `InputCallScriptFunction`
    /// (`baseentity.cpp:7373-7391`). Returns false if there is no VM.
    pub(super) fn script_input(&mut self, id: EntityId, input: &str, value: &str) -> bool {
        let input = input.to_ascii_lowercase();
        self.with_vm(|vm, server| match input.as_str() {
            "runscriptfile" => {
                server.run_entity_script_file(vm, id, value, false);
            }
            "runscriptcode" => server.run_script_code(vm, id, value),
            _ => {
                server.call_script_function(vm, id, value);
            }
        })
        .is_some()
    }

    /// `CBaseEntity::ScriptThink` (`baseentity.cpp:7542`) for every entity
    /// whose `thinkfunction` is due. Runs after the ordinary thinks, in the
    /// order the contexts were set.
    pub(super) fn run_script_thinks(&mut self) {
        if self.script.thinks.is_empty() {
            return;
        }
        let tick = self.clock.time().tick;
        let due: Vec<ScriptThink> = self
            .script
            .thinks
            .iter()
            .filter(|t| t.tick <= tick)
            .cloned()
            .collect();
        for think in due {
            let alive = self.entities.get(think.entity).is_some_and(|e| !e.removed);
            if !alive {
                continue;
            }
            let result = self
                .with_vm(|vm, server| server.call_script_function(vm, think.entity, &think.function))
                .flatten();
            let time = self.clock.time();
            match result {
                Some(value) => {
                    // `AssignTo( &flThinkFrequency )` — a number, or the
                    // default interval.
                    let interval = match value {
                        Value::Float(f) => f,
                        Value::Integer(i) => i as f32,
                        Value::Bool(b) => b as i32 as f32,
                        _ => SCRIPT_THINK_INTERVAL,
                    };
                    if let Some(t) = self.script.thinks.iter_mut().find(|t| t.entity == think.entity) {
                        t.tick = time.time_to_ticks(time.curtime + interval);
                    }
                }
                None => {
                    let name = self.debug_name(think.entity);
                    self.script_warning(&format!(
                        "{name} FAILED to call script think function {}!",
                        think.function
                    ));
                    self.script.thinks.retain(|t| t.entity != think.entity);
                }
            }
        }
    }

    // ----- natives' helpers -----------------------------------------------------

    /// `DoEntFire` and `EntFireByHandle`'s shared tail — `g_EventQueue.AddEvent`.
    fn add_script_event(
        &mut self,
        target: Target,
        action: &str,
        value: &str,
        delay: f32,
        activator: Option<EntityId>,
        caller: Option<EntityId>,
    ) {
        let input = match action.is_empty() {
            true => "Use".to_owned(),
            false => action.to_owned(),
        };
        let value = match value.is_empty() {
            true => Variant::Void,
            false => Variant::String(value.to_owned()),
        };
        let fire_time = self.clock.time().curtime + delay.max(0.0);
        self.queue.add(Event {
            fire_time,
            target,
            input,
            value,
            activator,
            caller,
            output_id: 0,
        });
    }

    /// `CBaseEntity::Teleport`, as the natives use it.
    fn script_teleport(&mut self, id: EntityId, teleport: Teleport) {
        if let Some(entity) = self.entities.get_mut(id) {
            teleport.apply(&mut entity.core);
            entity.core.flags &= !FL_ONGROUND;
        }
        let now = self.clock.time().curtime;
        hierarchy::propagate_id(
            id,
            &mut self.entities,
            attachment::Poser {
                attachments: self.attachments.as_ref(),
                now,
            },
        );
    }

    /// `gEntList.NextEnt` from a handle, or the first.
    fn entities_after(&self, start: Option<EntityId>) -> Vec<EntityId> {
        let mut ids = self.entities.iter().map(|(id, _)| id);
        match start {
            None => ids.collect(),
            Some(start) => {
                let mut seen = false;
                ids.by_ref()
                    .filter(|&id| {
                        if seen {
                            return true;
                        }
                        if id == start {
                            seen = true;
                        }
                        false
                    })
                    .collect()
            }
        }
    }
}

/// `CLogicScript`'s embedded `szAddCode` (`logicentities.cpp:46`).
const ENTITY_GROUP_NUT: &[u8] = b"EntityGroup <- [];\r\nfunction __AppendToScriptGroup( name ) \r\n{\r\n\tif ( name.len() == 0 ) \r\n\t{ \r\n\t\tEntityGroup.append( null ); \r\n\t} \r\n\telse\r\n\t{ \r\n\t\tlocal ent = Entities.FindByName( null, name );\r\n\t\tEntityGroup.append( ent );\r\n\t\tif ( ent != null )\r\n\t\t{\r\n\t\t\tent.ValidateScriptScope();\r\n\t\t\tent.GetScriptScope().EntityGroup <- EntityGroup;\r\n\t\t}\r\n\t} \r\n}\r\n";

/// `vscript_server.nut`, as compiled into Valve's server
/// (`g_Script_vscript_server`, `vscript_server.cpp:22`) — byte for byte the
/// same as the `.nut` beside it in the C++ tree.
const VSCRIPT_SERVER_NUT: &[u8] = include_bytes!("vscript_server.nut");

// ----- the natives ---------------------------------------------------------------

fn register_globals(vm: &mut Vm) {
    let root = vm.root();
    // `CScriptManager::CreateVM`'s two (`vscript.cpp:57`) — the global
    // uniform random stream, which is the level's here.
    vm.register_native(&root, "RandomFloat", 3, ".nn", |_, host, a| {
        let s = server(host);
        Ok(Value::Float(s.random.float(arg(a, 1).to_float(), arg(a, 2).to_float())))
    });
    vm.register_native(&root, "RandomInt", 3, ".nn", |_, host, a| {
        let s = server(host);
        Ok(Value::Integer(s.random.int(arg(a, 1).to_integer(), arg(a, 2).to_integer())))
    });
    vm.register_native(&root, "ShowMessage", 2, ".s", |vm, _, a| {
        // `UTIL_ShowMessageAll` is a HUD message; there is no HUD, so it goes
        // to the console.
        vm.print(&format!("{}\n", string_arg(a, 1)));
        Ok(Value::Null)
    });
    vm.register_native(&root, "SendToConsole", 2, ".s", |_, host, a| {
        server(host).console_commands.push(string_arg(a, 1));
        Ok(Value::Null)
    });
    vm.register_native(&root, "SendToConsoleServer", 2, ".s", |_, host, a| {
        server(host).console_commands.push(string_arg(a, 1));
        Ok(Value::Null)
    });
    vm.register_native(&root, "GetMapName", 1, ".", |_, host, _| {
        Ok(Value::str(server(host).map.as_deref().unwrap_or("")))
    });
    // `loopsingleplayermaps` — a cvar this port does not have, at its
    // default.
    vm.register_native(&root, "LoopSinglePlayerMaps", 1, ".", |_, _, _| Ok(Value::Bool(false)));
    vm.register_native(&root, "Time", 1, ".", |_, host, _| {
        Ok(Value::Float(server(host).clock.time().curtime))
    });
    vm.register_native(&root, "FrameTime", 1, ".", |_, host, _| {
        Ok(Value::Float(server(host).clock.time().interval))
    });
    // `DoEntFire`, aliased `EntFire` — and then replaced by
    // `vscript_server.nut`'s script `EntFire`, which calls it.
    let do_ent_fire = |_: &mut Vm, host: &mut dyn Any, a: &[Value]| -> Result<Value, Value> {
        let s = server(host);
        let target = string_arg(a, 1);
        s.add_script_event(
            Target::Name(target),
            &string_arg(a, 2),
            &string_arg(a, 3),
            arg(a, 4).to_float(),
            to_ent(&arg(a, 5)),
            to_ent(&arg(a, 6)),
        );
        Ok(Value::Null)
    };
    vm.register_native(&root, "DoEntFire", 7, ".sssn..", do_ent_fire);
    vm.register_native(&root, "EntFire", 7, ".sssn..", do_ent_fire);
    vm.register_native(&root, "EntFireByHandle", 7, "..ssn..", |_, host, a| {
        let s = server(host);
        let Some(target) = to_ent(&arg(a, 1)).filter(|&id| s.entities.is_alive(id)) else {
            eprintln!("source-engine: vscript: VScript error: DoEntFire was passed an invalid entity instance.");
            return Ok(Value::Null);
        };
        s.add_script_event(
            Target::Entity(target),
            &string_arg(a, 2),
            &string_arg(a, 3),
            arg(a, 4).to_float(),
            to_ent(&arg(a, 5)),
            to_ent(&arg(a, 6)),
        );
        Ok(Value::Null)
    });
    let unique = |_: &mut Vm, host: &mut dyn Any, a: &[Value]| -> Result<Value, Value> {
        Ok(Value::str(&server(host).generate_unique_key(&string_arg(a, 1))))
    };
    vm.register_native(&root, "DoUniqueString", 2, ".s", unique);
    vm.register_native(&root, "UniqueString", 2, ".s", unique);
    // The debug overlay, achievements, particles and the audio mixer are not
    // ported; these calls change nothing a script or a map can observe.
    vm.register_native(&root, "DebugDrawBox", 9, ".xxxnnnnn", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "DebugDrawLine", 8, ".xxnnnbn", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "RecordAchievementEvent", 3, ".sn", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "DispatchParticleEffect", 4, ".sxx", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "SetDucking", 4, ".ssn", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "GetDeveloperLevel", 1, ".", |vm, _, _| Ok(Value::Integer(vm.developer())));
    vm.register_native(&root, "DoIncludeScript", 3, ".s.", |vm, host, a| {
        let s = server(host);
        let name = string_arg(a, 1);
        let scope = match arg(a, 2) {
            Value::Null => None,
            other => Some(other),
        };
        if !s.run_script_file(vm, &name, scope, true) {
            return Err(Value::str(&format!("Failed to include script \"{name}\"")));
        }
        Ok(Value::Bool(true))
    });
    register_game_rules(vm);
}

/// `CPortalGameRules::RegisterScriptFunctions` (`portal_gamerules.cpp:433`)
/// — Portal 2's single-player rules, which is all this port runs; a co-op
/// map is played by one player under them.
///
/// **Most of these are co-op functions, and they are ported exactly rather
/// than stubbed**: each is a free function in `portal_mp_gamerules.cpp` that
/// begins `if ( !PortalMPGameRules() ) return …`, and in single player there
/// is no multiplayer rules object — so `AddBranchLevelName` does nothing,
/// `IsLevelComplete` is false and the co-op indices are 0, in the shipped game
/// as here. 85 `AddBranchLevelName` calls on the co-op maps and 68
/// `PrecacheMovie`s on the elevator video scripts were the two commonest
/// errors across the game before these were registered.
///
/// Not registered, because they need a system that is not here:
/// `GetPlayerSilenceDuration` and the `PlayerVoiceListener` instance (voice
/// chat), `ScriptSteamShowURL` (the Steam overlay), `ScriptShowHudMessageAll`
/// (the HUD), and `GivePlayerPortalgun` with its two upgrades (the weapon).
fn register_game_rules(vm: &mut Vm) {
    let root = vm.root();
    // `ScriptIsMultiplayer` — `return false;//g_pGameRules->IsMultiplayer();`.
    vm.register_native(&root, "IsMultiplayer", 1, ".", |_, _, _| Ok(Value::Bool(false)));
    // `GetTeamPlayerByIndex( TEAM_RED / TEAM_BLUE )`: the entity index of the
    // first player on that team, or -1. The single-player player is on
    // neither.
    vm.register_native(&root, "GetOrangePlayerIndex", 1, ".", |_, _, _| Ok(Value::Integer(-1)));
    vm.register_native(&root, "GetBluePlayerIndex", 1, ".", |_, _, _| Ok(Value::Integer(-1)));
    vm.register_native(&root, "GetCoopSectionIndex", 1, ".", |_, _, _| Ok(Value::Integer(0)));
    vm.register_native(&root, "GetCoopBranchLevelIndex", 2, ".n", |_, _, _| Ok(Value::Integer(0)));
    vm.register_native(&root, "GetHighestActiveBranch", 1, ".", |_, _, _| Ok(Value::Integer(0)));
    vm.register_native(&root, "AddBranchLevelName", 3, ".ns", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "MarkMapComplete", 2, ".s", |_, _, _| Ok(Value::Null));
    vm.register_native(&root, "IsLevelComplete", 3, ".nn", |_, _, _| Ok(Value::Bool(false)));
    vm.register_native(&root, "IsPlayerLevelComplete", 4, ".nnn", |_, _, _| Ok(Value::Bool(false)));
    vm.register_native(&root, "AddCoopCreditsName", 2, ".s", |_, _, _| Ok(Value::Null));
    // `GetPlayer` — `ToHScript( UTIL_GetLocalPlayer() )`.
    vm.register_native(&root, "GetPlayer", 1, ".", |vm, host, _| {
        let s = server(host);
        Ok(match s.player {
            Some(id) => s.script_instance(vm, id),
            None => Value::Null,
        })
    });
    // `PrecacheMovie` adds a name to the movie string table the client reads
    // before it plays one. There is no movie playback, and nothing a script
    // can see changes.
    vm.register_native(&root, "PrecacheMovie", 2, ".s", |_, _, _| Ok(Value::Null));
    // "Tests if the DLC1 is installed for Try/Catch blocks" — it is.
    vm.register_native(&root, "TryDLC1InstalledOrCatch", 1, ".", |_, _, _| Ok(Value::Bool(true)));
}

/// The `CBaseEntity` script class (`baseentity.cpp:2459`), with the members
/// `CSquirrelVM::RegisterClass` adds to every class: `_tostring` and
/// `IsValid`.
fn register_entity_class(vm: &mut Vm) -> ClassRef {
    type NativeFn = fn(&mut Vm, &mut dyn Any, &[Value]) -> Result<Value, Value>;

    /// The entity `this` names, or the error `TranslateCall` raises.
    fn this_entity(host: &mut dyn Any, a: &[Value]) -> Result<EntityId, Value> {
        let id = to_ent(&arg(a, 0)).ok_or_else(|| Value::str("Accessed null instance"))?;
        match server(host).entities.is_alive(id) {
            true => Ok(id),
            false => Err(Value::str("Accessed null instance")),
        }
    }

    let class = vm.new_class(None, 0);
    let methods: &[(&str, i32, &str, NativeFn)] = &[
        ("_tostring", 0, "", |_, host, a| {
            let s = server(host);
            let Some(id) = to_ent(&arg(a, 0)) else {
                return Ok(Value::str(&format!("(instance : 0x{:08X})", arg(a, 0).address() as u32)));
            };
            let Some(e) = s.entities.get(id) else {
                return Ok(Value::str(&format!("(instance : 0x{:08X})", arg(a, 0).address() as u32)));
            };
            Ok(Value::str(&match &e.name {
                Some(name) => format!("([{}] {}: {})", id.slot(), e.classname(), name),
                None => format!("([{}] {})", id.slot(), e.classname()),
            }))
        }),
        ("IsValid", 0, "", |_, host, a| {
            let alive = to_ent(&arg(a, 0)).is_some_and(|id| server(host).entities.is_alive(id));
            Ok(Value::Bool(alive))
        }),
        ("GetClassname", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            Ok(Value::str(server(host).entities.get(id).map_or("", |e| e.classname())))
        }),
        ("GetName", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            let name = server(host).entities.get(id).and_then(|e| e.name.clone());
            Ok(Value::str(name.as_deref().unwrap_or("")))
        }),
        ("GetPreTemplateName", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            let name = server(host).entities.get(id).and_then(|e| e.name.clone()).unwrap_or_default();
            let stripped = match name.rfind('&') {
                Some(i) => name[..i.min(127)].to_owned(),
                None => name,
            };
            Ok(Value::str(&stripped))
        }),
        ("GetOrigin", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let origin = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.origin);
            Ok(vscript::vector_value(vm, origin.into()))
        }),
        ("SetAbsOrigin", 2, ".x", |_, host, a| {
            let id = this_entity(host, a)?;
            let origin = vector_arg(a, 1)?;
            let s = server(host);
            if let Some(e) = s.entities.get_mut(id) {
                e.core.set_abs_origin(origin);
            }
            let now = s.clock.time().curtime;
            hierarchy::propagate_id(id, &mut s.entities, attachment::Poser { attachments: s.attachments.as_ref(), now });
            Ok(Value::Null)
        }),
        ("SetOrigin", 2, ".x", |_, host, a| {
            let id = this_entity(host, a)?;
            let origin = vector_arg(a, 1)?;
            server(host).script_teleport(id, Teleport { origin: Some(origin), angles: None, velocity: None });
            Ok(Value::Null)
        }),
        ("GetAngles", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let angles = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.angles);
            Ok(vscript::vector_value(vm, angles.into()))
        }),
        ("SetAngles", 4, ".nnn", |_, host, a| {
            let id = this_entity(host, a)?;
            let angles = Vec3::new(arg(a, 1).to_float(), arg(a, 2).to_float(), arg(a, 3).to_float());
            server(host).script_teleport(id, Teleport { origin: None, angles: Some(angles), velocity: None });
            Ok(Value::Null)
        }),
        ("GetForwardVector", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let angles = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.angles);
            Ok(vscript::vector_value(vm, crate::math::angle_vectors(angles).0.into()))
        }),
        // `ScriptGetLeft` asks `GetVectors` for its *second* vector, which is
        // the right vector — so `GetLeftVector` points right. Valve's, kept.
        ("GetLeftVector", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let angles = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.angles);
            Ok(vscript::vector_value(vm, crate::math::angle_vectors(angles).1.into()))
        }),
        ("GetUpVector", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let angles = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.angles);
            Ok(vscript::vector_value(vm, crate::math::angle_vectors(angles).2.into()))
        }),
        ("SetForwardVector", 2, ".x", |_, host, a| {
            let id = this_entity(host, a)?;
            let forward = vector_arg(a, 1)?;
            let angles = crate::math::vector_angles_forward(forward);
            server(host).script_teleport(id, Teleport { origin: None, angles: Some(angles), velocity: None });
            Ok(Value::Null)
        }),
        ("GetVelocity", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let v = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.velocity);
            Ok(vscript::vector_value(vm, v.into()))
        }),
        ("SetVelocity", 2, ".x", |_, host, a| {
            let id = this_entity(host, a)?;
            let v = vector_arg(a, 1)?;
            if let Some(e) = server(host).entities.get_mut(id) {
                e.core.velocity = v;
            }
            Ok(Value::Null)
        }),
        ("GetCenter", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let center = server(host).entities.get(id).map_or(Vec3::ZERO, |e| {
                let (lo, hi) = e.world_space_aabb();
                (lo + hi) * 0.5
            });
            Ok(vscript::vector_value(vm, center.into()))
        }),
        ("EyePosition", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let s = server(host);
            let offset = match s.player == Some(id) {
                true => s.player_view_offset,
                false => Vec3::ZERO,
            };
            let eye = s.entities.get(id).map_or(Vec3::ZERO, |e| e.origin + offset);
            Ok(vscript::vector_value(vm, eye.into()))
        }),
        ("GetBoundingMins", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let v = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.model_bounds.mins);
            Ok(vscript::vector_value(vm, v.into()))
        }),
        ("GetBoundingMaxs", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let v = server(host).entities.get(id).map_or(Vec3::ZERO, |e| e.model_bounds.maxs);
            Ok(vscript::vector_value(vm, v.into()))
        }),
        ("Destroy", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            if let Some(e) = server(host).entities.get_mut(id) {
                e.core.remove();
            }
            Ok(Value::Null)
        }),
        ("GetHealth", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            Ok(Value::Integer(server(host).entities.get(id).map_or(0, |e| e.health)))
        }),
        ("SetHealth", 2, ".n", |_, host, a| {
            let id = this_entity(host, a)?;
            if let Some(e) = server(host).entities.get_mut(id) {
                e.core.health = arg(a, 1).to_integer();
            }
            Ok(Value::Null)
        }),
        ("GetMaxHealth", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            Ok(Value::Integer(server(host).entities.get(id).map_or(0, |e| e.max_health)))
        }),
        ("SetMaxHealth", 2, ".n", |_, host, a| {
            let id = this_entity(host, a)?;
            if let Some(e) = server(host).entities.get_mut(id) {
                e.core.max_health = arg(a, 1).to_integer();
            }
            Ok(Value::Null)
        }),
        ("GetModelName", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            let model = server(host).entities.get(id).and_then(|e| e.model.clone());
            Ok(Value::str(model.as_deref().unwrap_or("")))
        }),
        ("GetMoveParent", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let s = server(host);
            match s.entities.get(id).and_then(|e| e.parent()) {
                Some(p) => Ok(s.script_instance(vm, p)),
                None => Ok(Value::Null),
            }
        }),
        ("GetRootMoveParent", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let s = server(host);
            let mut root = id;
            while let Some(p) = s.entities.get(root).and_then(|e| e.parent()) {
                root = p;
            }
            Ok(s.script_instance(vm, root))
        }),
        ("FirstMoveChild", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let s = server(host);
            match s.entities.get(id).and_then(|e| e.children().first().copied()) {
                Some(c) => Ok(s.script_instance(vm, c)),
                None => Ok(Value::Null),
            }
        }),
        ("NextMovePeer", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            let s = server(host);
            let parent = s.entities.get(id).and_then(|e| e.parent());
            let next = parent.and_then(|p| s.entities.get(p)).and_then(|p| {
                let children = p.children();
                let i = children.iter().position(|&c| c == id)?;
                children.get(i + 1).copied()
            });
            match next {
                Some(n) => Ok(s.script_instance(vm, n)),
                None => Ok(Value::Null),
            }
        }),
        ("ValidateScriptScope", 1, ".", |vm, host, a| {
            let id = this_entity(host, a)?;
            Ok(Value::Bool(server(host).validate_script_scope(vm, id).is_some()))
        }),
        ("GetScriptScope", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            Ok(server(host).script.scopes.get(&id).cloned().map_or(Value::Null, Value::Table))
        }),
        // `CBaseEntity::GetScriptId` returns `m_iszScriptThinkFunction` — a
        // Valve bug, kept.
        ("GetScriptId", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            let think = server(host).entities.get(id).and_then(|e| e.script_think_function.clone());
            Ok(Value::str(think.as_deref().unwrap_or("")))
        }),
        ("entindex", 1, ".", |_, host, a| {
            let id = this_entity(host, a)?;
            Ok(Value::Integer(id.slot() as i32))
        }),
        // No sound system: these are the calls that would make a noise, and
        // precaching one is a hint to a cache that does not exist.
        ("EmitSound", 2, ".s", |_, host, a| this_entity(host, a).map(|_| Value::Null)),
        ("StopSound", 2, ".s", |_, host, a| this_entity(host, a).map(|_| Value::Null)),
        ("PrecacheSoundScript", 2, ".s", |_, host, a| this_entity(host, a).map(|_| Value::Null)),
        ("PrecacheScriptSound", 2, ".s", |_, host, a| this_entity(host, a).map(|_| Value::Null)),
        ("PrecacheModel", 2, ".s", |_, host, a| this_entity(host, a).map(|_| Value::Null)),
        ("ConnectOutput", 3, ".ss", |_, host, a| {
            let id = this_entity(host, a)?;
            server(host).connect_output_to_script(id, &string_arg(a, 1), &string_arg(a, 2), true);
            Ok(Value::Null)
        }),
        ("DisconnectOutput", 3, ".ss", |_, host, a| {
            let id = this_entity(host, a)?;
            server(host).connect_output_to_script(id, &string_arg(a, 1), &string_arg(a, 2), false);
            Ok(Value::Null)
        }),
        ("__KeyValueFromString", 3, ".ss", |_, host, a| {
            let id = this_entity(host, a)?;
            Ok(Value::Bool(server(host).script_key_value(id, &string_arg(a, 1), &string_arg(a, 2))))
        }),
        ("__KeyValueFromFloat", 3, ".sn", |_, host, a| {
            let id = this_entity(host, a)?;
            let value = format!("{:.6}", arg(a, 2).to_float());
            Ok(Value::Bool(server(host).script_key_value(id, &string_arg(a, 1), &value)))
        }),
        ("__KeyValueFromInt", 3, ".sn", |_, host, a| {
            let id = this_entity(host, a)?;
            let value = arg(a, 2).to_integer().to_string();
            Ok(Value::Bool(server(host).script_key_value(id, &string_arg(a, 1), &value)))
        }),
        ("__KeyValueFromVector", 3, ".sx", |_, host, a| {
            let id = this_entity(host, a)?;
            let v = vector_arg(a, 2)?;
            let value = format!("{:.6} {:.6} {:.6}", v.x, v.y, v.z);
            Ok(Value::Bool(server(host).script_key_value(id, &string_arg(a, 1), &value)))
        }),
    ];
    for (name, nparams, mask, func) in methods {
        let native = vm.native(name, *nparams, mask, *func);
        vm.class_new_slot(&class, name, native);
    }
    let root = vm.root();
    vm.set_slot(&root, "CBaseEntity", Value::Class(class.clone()));
    class
}

/// How the player's script class is recognised.
const PLAYER_TYPE_TAG: usize = 0x5041_4c59;

/// The `CBasePlayer` script class (`player.cpp:483`). Valve's derives from
/// `CBaseAnimating`'s, whose members are not ported, so this one derives
/// straight from `CBaseEntity`'s.
fn register_player_class(vm: &mut Vm, base: &ClassRef) -> ClassRef {
    let class = vm.new_class(Some(base.clone()), PLAYER_TYPE_TAG);
    let noclip = vm.native("IsNoclipping", 1, ".", |_, host, a| {
        let s = server(host);
        let id = to_ent(&arg(a, 0)).filter(|&id| s.entities.is_alive(id));
        let Some(id) = id else {
            return Err(Value::str("Accessed null instance"));
        };
        let noclip = s
            .entities
            .get(id)
            .is_some_and(|e| e.move_type == super::movement::MoveType::Noclip);
        Ok(Value::Bool(noclip))
    });
    vm.class_new_slot(&class, "IsNoclipping", noclip);
    let root = vm.root();
    vm.set_slot(&root, "CBasePlayer", Value::Class(class.clone()));
    class
}

/// `CScriptEntityIterator`, registered as the instance `Entities`.
fn register_entities_singleton(vm: &mut Vm) {
    type NativeFn = fn(&mut Vm, &mut dyn Any, &[Value]) -> Result<Value, Value>;

    fn instance_or_null(vm: &mut Vm, s: &mut Server, id: Option<EntityId>) -> Value {
        match id {
            Some(id) => s.script_instance(vm, id),
            None => Value::Null,
        }
    }

    let class = vm.new_class(None, 0);
    let methods: &[(&str, i32, &str, NativeFn)] = &[
        ("First", 1, ".", |vm, host, _| {
            let s = server(host);
            let first = s.entities_after(None).first().copied();
            Ok(instance_or_null(vm, s, first))
        }),
        ("Next", 2, "..", |vm, host, a| {
            let s = server(host);
            let next = s.entities_after(to_ent(&arg(a, 1))).first().copied();
            Ok(instance_or_null(vm, s, next))
        }),
        ("FindByClassname", 3, "..s", |vm, host, a| {
            let s = server(host);
            let class = string_arg(a, 2);
            let found = s
                .entities_after(to_ent(&arg(a, 1)))
                .into_iter()
                .find(|&id| s.entities.get(id).is_some_and(|e| name::names_match(&class, e.classname())));
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindByName", 3, "..s", |vm, host, a| {
            let s = server(host);
            let start = to_ent(&arg(a, 1));
            let name = string_arg(a, 2);
            Ok(match s.find_entity_by_name(start, &name) {
                Some(id) => s.script_instance(vm, id),
                None => Value::Null,
            })
        }),
        ("FindByTarget", 3, "..s", |vm, host, a| {
            let s = server(host);
            let target = string_arg(a, 2);
            let found = s
                .entities_after(to_ent(&arg(a, 1)))
                .into_iter()
                .find(|&id| s.entities.get(id).and_then(|e| e.target.as_deref()) == Some(target.as_str()));
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindByModel", 3, "..s", |vm, host, a| {
            let s = server(host);
            let model = string_arg(a, 2);
            let found = s
                .entities_after(to_ent(&arg(a, 1)))
                .into_iter()
                .find(|&id| {
                    s.entities
                        .get(id)
                        .and_then(|e| e.model.as_deref())
                        .is_some_and(|m| m.eq_ignore_ascii_case(&model))
                });
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindInSphere", 4, "..xn", |vm, host, a| {
            let s = server(host);
            let center = vector_arg(a, 2)?;
            let radius = arg(a, 3).to_float();
            let found = s.entities_after(to_ent(&arg(a, 1))).into_iter().find(|&id| {
                s.entities.get(id).is_some_and(|e| {
                    let (lo, hi) = e.world_space_aabb();
                    let nearest = center.clamp(lo, hi);
                    (nearest - center).length_squared() <= radius * radius
                })
            });
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindByNameNearest", 4, ".sxn", |vm, host, a| {
            let s = server(host);
            let found = s.find_nearest(&string_arg(a, 1), vector_arg(a, 2)?, arg(a, 3).to_float(), true);
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindByNameWithin", 5, "..sxn", |vm, host, a| {
            let s = server(host);
            let found = s.find_within(to_ent(&arg(a, 1)), &string_arg(a, 2), vector_arg(a, 3)?, arg(a, 4).to_float(), true);
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindByClassnameNearest", 4, ".sxn", |vm, host, a| {
            let s = server(host);
            let found = s.find_nearest(&string_arg(a, 1), vector_arg(a, 2)?, arg(a, 3).to_float(), false);
            Ok(instance_or_null(vm, s, found))
        }),
        ("FindByClassnameWithin", 5, "..sxn", |vm, host, a| {
            let s = server(host);
            let found = s.find_within(to_ent(&arg(a, 1)), &string_arg(a, 2), vector_arg(a, 3)?, arg(a, 4).to_float(), false);
            Ok(instance_or_null(vm, s, found))
        }),
    ];
    for (name, nparams, mask, func) in methods {
        let native = vm.native(name, *nparams, mask, *func);
        vm.class_new_slot(&class, name, native);
    }
    let root = vm.root();
    vm.set_slot(&root, "CEntities", Value::Class(class.clone()));
    let instance = vm.new_instance(&class, None);
    vm.set_slot(&root, "Entities", Value::Instance(instance));
}

impl Server {
    /// `CGlobalEntityList::FindEntityByName` from a start entity: a `!name`
    /// resolves only when starting afresh, "to avoid an infinite loop".
    fn find_entity_by_name(&self, start: Option<EntityId>, query: &str) -> Option<EntityId> {
        if query.is_empty() {
            return None;
        }
        if name::is_procedural(query) {
            if start.is_some() {
                return None;
            }
            return match name::find_procedural(query, None, None, None, self.player) {
                Procedural::Resolved(found) => found,
                _ => None,
            };
        }
        self.entities_after(start).into_iter().find(|&id| {
            self.entities
                .get(id)
                .and_then(|e| e.name.as_deref())
                .is_some_and(|n| name::names_match(query, n))
        })
    }

    /// `FindEntityByNameNearest` / `FindEntityByClassnameNearest`: a radius of
    /// zero is `MAX_TRACE_LENGTH`.
    fn find_nearest(&self, query: &str, source: Vec3, radius: f32, by_name: bool) -> Option<EntityId> {
        let mut best = None;
        let mut max = radius * radius;
        if max == 0.0 {
            // `MAX_TRACE_LENGTH` — `1.732050807569 * COORD_EXTENT`.
            let length = 1.732_050_8_f32 * 32768.0;
            max = length * length;
        }
        for id in self.entities_after(None) {
            let Some(e) = self.entities.get(id) else { continue };
            let matches = match by_name {
                true => e.name.as_deref().is_some_and(|n| name::names_match(query, n)),
                false => name::names_match(query, e.classname()),
            };
            if !matches {
                continue;
            }
            let d = (e.origin - source).length_squared();
            if max > d {
                best = Some(id);
                max = d;
            }
        }
        best
    }

    /// `FindEntityByNameWithin` / `FindEntityByClassnameWithin`.
    fn find_within(&self, start: Option<EntityId>, query: &str, source: Vec3, radius: f32, by_name: bool) -> Option<EntityId> {
        let max = radius * radius;
        self.entities_after(start).into_iter().find(|&id| {
            let Some(e) = self.entities.get(id) else {
                return false;
            };
            let matches = match by_name {
                true => e.name.as_deref().is_some_and(|n| name::names_match(query, n)),
                false => name::names_match(query, e.classname()),
            };
            matches && (max == 0.0 || max > (e.origin - source).length_squared())
        })
    }

    /// `ConnectOutputToScript` / `DisconnectOutputFromScript`
    /// (`baseentity.cpp:7480`): a `!self` → `CallScriptFunction` connection on
    /// one of the class's declared outputs.
    fn connect_output_to_script(&mut self, id: EntityId, output: &str, function: &str, connect: bool) {
        self.next_output_id += 1;
        let stamp = self.next_output_id;
        let Some(entity) = self.entities.get_mut(id) else {
            return;
        };
        let declared = entity.core.class.declared_output(output).or_else(|| {
            keyvalue::BASE_OUTPUTS
                .iter()
                .find(|name| name.eq_ignore_ascii_case(output))
                .copied()
        });
        let Some(declared) = declared else {
            return;
        };
        let is_ours = |a: &EventAction| {
            a.target == "!self"
                && a.delay == 0.0
                && a.times_to_fire == EVENT_FIRE_ALWAYS
                && a.input == "CallScriptFunction"
                && a.parameter.as_deref() == Some(function)
        };
        let existing = entity
            .core
            .outputs
            .iter()
            .position(|o| o.name.eq_ignore_ascii_case(declared));
        match connect {
            true => {
                if existing.is_some_and(|i| entity.core.outputs[i].actions.iter().any(is_ours)) {
                    return;
                }
                let action = EventAction {
                    target: "!self".into(),
                    input: "CallScriptFunction".into(),
                    parameter: Some(function.to_owned()),
                    delay: 0.0,
                    times_to_fire: EVENT_FIRE_ALWAYS,
                    id: stamp,
                };
                match existing {
                    Some(i) => entity.core.outputs[i].add(action),
                    None => {
                        let mut o = super::io::Output::new(declared);
                        o.add(action);
                        entity.core.outputs.push(o);
                    }
                }
            }
            false => {
                if let Some(i) = existing {
                    let actions = &mut entity.core.outputs[i].actions;
                    if let Some(at) = actions.iter().position(is_ours) {
                        actions.remove(at);
                    }
                }
            }
        }
    }

    /// `KeyValue` from a script — the class's half, then `CBaseEntity`'s.
    fn script_key_value(&mut self, id: EntityId, key: &str, value: &str) -> bool {
        let Some(entity) = self.entities.get_mut(id) else {
            return false;
        };
        let super::entity::Entity { core, behaviour } = &mut *entity;
        behaviour.key_value(core, key, value) || keyvalue::base_key_value(core, key, value)
    }
}
