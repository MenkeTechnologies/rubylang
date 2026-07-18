//! The Ruby object heap and runtime, reached from fusevm through registered
//! builtins (`register_builtin`) and the strict numeric hook.
//!
//! rubylang owns no VM and no JIT: the compiler lowers Ruby to `fusevm::Chunk`,
//! and every Ruby-specific operation the VM can't do natively is a builtin call
//! that lands here. Because the host is a thread-local, a method or block body
//! run as a *nested* fusevm VM automatically shares the caller's lexical scope
//! — which is exactly Ruby's block-capture semantics, for free.
//!
//! Value representation:
//!   - immediate: `Value::Int` (Integer), `Value::Float` (Float),
//!     `Value::Bool` (true/false), `Value::Undef` (nil);
//!   - heap `Value::Obj(u32)` handles: String, Array, Hash, Symbol, Range, Proc
//!     — the reference types, so `a.push(x)` mutates in place like real Ruby.

use fusevm::{Chunk, NumOp, VMResult, Value, VM};
use indexmap::IndexMap;
use std::cell::RefCell;

/// Builtin ids emitted by the compiler and registered on every VM.
pub mod ops {
    pub const GETLOCAL: u16 = 1; // [name] -> value
    pub const SETLOCAL: u16 = 2; // [name, value] -> value
    pub const GETIVAR: u16 = 3;
    pub const SETIVAR: u16 = 4;
    pub const GETGVAR: u16 = 5;
    pub const SETGVAR: u16 = 6;
    pub const GETCONST: u16 = 7;
    pub const SETCONST: u16 = 8;
    pub const CALL: u16 = 9; // [name, args...] argc=1+n     -> self/top-level
    pub const CALL_BLK: u16 = 10; // [name, args..., proc] argc=2+n
    pub const CALL_METHOD: u16 = 11; // [recv, name, args...] argc=2+n
    pub const CALL_METHOD_BLK: u16 = 12; // [recv, name, args..., proc] argc=3+n
    pub const MKSTR: u16 = 13; // [parts...] argc=n -> heap String
    pub const MKSYM: u16 = 14; // [name] -> Symbol
    pub const MKARRAY: u16 = 15; // [items...] argc=n -> heap Array
    pub const MKHASH: u16 = 16; // [k,v,...] argc=2n -> heap Hash
    pub const MKRANGE: u16 = 17; // [lo, hi, exclusive] -> Range
    pub const MKPROC: u16 = 18; // [proc_id] -> Proc
    pub const YIELD: u16 = 19; // [args...] argc=n -> block result
    pub const TRUTHY: u16 = 20; // [v] -> Bool (Ruby: only nil/false are falsy)
    pub const INDEX_GET: u16 = 21; // [recv, idx...] argc=1+n
    pub const INDEX_SET: u16 = 22; // [recv, idx..., val] argc=2+n
    pub const TOSTR: u16 = 23; // [v] -> heap String (to_s, for interpolation)
    pub const DEFINED: u16 = 24; // [name] -> Bool (local defined?)
    pub const SIG_BREAK: u16 = 25; // [v] -> halt block, propagate break
    pub const SIG_NEXT: u16 = 26; // [v] -> halt block, block value = v
    pub const SIG_RETURN: u16 = 27; // [v] -> halt method, return v
    pub const GETSELF: u16 = 28; // [] -> current self
    pub const BEGIN: u16 = 29; // [begin_id] -> run begin/rescue/ensure
    pub const SUPER: u16 = 30; // [args...] argc=n -> super with explicit args
    pub const SUPER_FWD: u16 = 31; // [] -> super forwarding the current args
    pub const MKARGS: u16 = 32; // [arrays...] argc=n -> concatenated array (splat)
    pub const CALL_ARR: u16 = 33; // [name, args_array] -> self call, args spread
    pub const CALL_METHOD_ARR: u16 = 34; // [recv, name, args_array] -> method call
}

/// A heap object — the Ruby reference types.
#[derive(Debug, Clone)]
pub enum RObj {
    Str(String),
    Array(Vec<Value>),
    Hash(IndexMap<RKey, Value>),
    Symbol(String),
    Range {
        lo: i64,
        hi: i64,
        exclusive: bool,
    },
    /// A block/proc: its compiled template plus the frame index it was created
    /// in (Ruby blocks capture the *lexical* scope where they appear, so a block
    /// passed down into another method and `yield`ed still reads the enclosing
    /// method's locals, not the callee's).
    Proc {
        template: usize,
        frame: usize,
    },
    /// A user-defined object: its class name and its instance variables.
    Object {
        class: String,
        ivars: IndexMap<String, Value>,
    },
    /// A reference to a class/module (the value of a constant like `Foo`), used
    /// as the receiver of `Foo.new`, `Foo.name`, etc.
    ClassRef(String),
}

/// A user-defined class: its optional superclass, its instance methods, the
/// modules it `include`s (searched after own methods, before the superclass),
/// and its class methods (`def self.m`).
#[derive(Clone, Default)]
pub struct ClassDef {
    pub superclass: Option<String>,
    pub methods: IndexMap<String, MethodDef>,
    pub includes: Vec<String>,
    pub class_methods: IndexMap<String, MethodDef>,
}

/// A `begin`/`rescue`/`ensure` block, compiled to proc templates.
#[derive(Clone)]
pub struct BeginDef {
    pub body: usize,
    pub rescues: Vec<RescueDef>,
    pub ensure: Option<usize>,
}

/// One compiled `rescue` clause.
#[derive(Clone)]
pub struct RescueDef {
    pub classes: Vec<String>,
    pub binding: Option<String>,
    pub body: usize,
}

/// A hashable Ruby value used as a Hash key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RKey {
    Int(i64),
    Str(String),
    Sym(String),
    Bool(bool),
    Nil,
    FloatBits(u64),
}

/// A compiled method: positional parameter names, the index of a splat
/// (`*rest`) parameter if any, the keyword parameter names (`name:`), and the
/// body chunk. Keyword params are bound from a trailing keyword Hash argument.
#[derive(Clone)]
pub struct MethodDef {
    pub params: Vec<String>,
    pub splat: Option<usize>,
    pub kwparams: Vec<String>,
    /// `**opts` collector parameter name, if any.
    pub kwsplat: Option<String>,
    pub chunk: Chunk,
}

/// A compiled block template.
#[derive(Clone)]
pub struct ProcDef {
    pub params: Vec<String>,
    pub chunk: Chunk,
}

/// One lexical scope frame (a method activation, or the top level).
struct Frame {
    locals: IndexMap<String, Value>,
    block: Option<Value>,
    /// The receiver `self` this frame runs against (`Undef` = top-level main).
    self_obj: Value,
    /// The running method's name and the class it was defined in, plus the args
    /// it was called with — for `super` (which resumes the lookup above
    /// `def_class` and, bare, forwards these args).
    method_name: Option<String>,
    def_class: Option<String>,
    args: Vec<Value>,
}

/// A non-local control signal raised by `break`/`next`/`return` inside a block.
#[derive(Clone)]
enum Signal {
    Break(Value),
    Next(Value),
    Return(Value),
}

/// The Ruby runtime.
pub struct RubyHost {
    heap: Vec<RObj>,
    frames: Vec<Frame>,
    globals: IndexMap<String, Value>,
    consts: IndexMap<String, Value>,
    methods: IndexMap<String, MethodDef>,
    classes: IndexMap<String, ClassDef>,
    begins: Vec<BeginDef>,
    procs: Vec<ProcDef>,
    symbols: IndexMap<String, u32>,
    pub error: Option<String>,
    /// The exception object of the in-flight `raise`, if any (for `rescue`).
    pending_exc: Option<Value>,
    signal: Option<Signal>,
    /// The frame local variable access targets. `None` = the top of the frame
    /// stack (a method body / top level); `Some(i)` = a captured frame while a
    /// block created in frame `i` is running.
    active_frame: Option<usize>,
}

thread_local! {
    static HOST: RefCell<RubyHost> = RefCell::new(RubyHost::new());
}

/// Run `f` with mutable access to the thread-local host.
pub fn with_host<R>(f: impl FnOnce(&mut RubyHost) -> R) -> R {
    HOST.with(|h| f(&mut h.borrow_mut()))
}

/// Reset the host to a clean slate (fresh top-level frame).
pub fn reset_host() {
    with_host(|h| *h = RubyHost::new());
}

impl Default for RubyHost {
    fn default() -> Self {
        Self::new()
    }
}

impl RubyHost {
    pub fn new() -> Self {
        RubyHost {
            heap: Vec::new(),
            frames: vec![Frame {
                locals: IndexMap::new(),
                block: None,
                self_obj: Value::Undef,
                method_name: None,
                def_class: None,
                args: Vec::new(),
            }],
            globals: IndexMap::new(),
            consts: IndexMap::new(),
            methods: IndexMap::new(),
            classes: IndexMap::new(),
            begins: Vec::new(),
            procs: Vec::new(),
            symbols: IndexMap::new(),
            error: None,
            pending_exc: None,
            signal: None,
            active_frame: None,
        }
    }

    /// Install compiled methods, classes, begin-blocks, and block templates
    /// before running main.
    pub fn load_program(
        &mut self,
        methods: Vec<(String, MethodDef)>,
        classes: Vec<(String, ClassDef)>,
        begins: Vec<BeginDef>,
        procs: Vec<ProcDef>,
    ) {
        for (name, def) in methods {
            self.methods.insert(name, def);
        }
        for (name, def) in classes {
            self.classes.insert(name, def);
        }
        self.begins = begins;
        self.procs = procs;
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    // ---- heap helpers -----------------------------------------------------

    fn alloc(&mut self, obj: RObj) -> Value {
        let id = self.heap.len() as u32;
        self.heap.push(obj);
        Value::Obj(id)
    }
    fn obj(&self, v: &Value) -> Option<&RObj> {
        match v {
            Value::Obj(i) => self.heap.get(*i as usize),
            _ => None,
        }
    }
    fn obj_mut(&mut self, v: &Value) -> Option<&mut RObj> {
        match v {
            Value::Obj(i) => self.heap.get_mut(*i as usize),
            _ => None,
        }
    }
    pub fn new_string(&mut self, s: String) -> Value {
        self.alloc(RObj::Str(s))
    }
    pub fn new_array(&mut self, items: Vec<Value>) -> Value {
        self.alloc(RObj::Array(items))
    }
    pub fn new_hash(&mut self, map: IndexMap<RKey, Value>) -> Value {
        self.alloc(RObj::Hash(map))
    }
    pub fn new_range(&mut self, lo: i64, hi: i64, exclusive: bool) -> Value {
        self.alloc(RObj::Range { lo, hi, exclusive })
    }
    /// Create a proc capturing the currently-active frame as its lexical scope.
    pub fn new_proc(&mut self, template: usize) -> Value {
        let frame = self.active_idx();
        self.alloc(RObj::Proc { template, frame })
    }
    pub fn new_symbol(&mut self, name: &str) -> Value {
        self.intern(name)
    }

    // ---- public accessors used by builtins (fine-grained borrows) ---------

    pub fn as_array(&self, v: &Value) -> Option<Vec<Value>> {
        match self.obj(v) {
            Some(RObj::Array(xs)) => Some(xs.clone()),
            _ => None,
        }
    }
    pub fn set_array(&mut self, v: &Value, xs: Vec<Value>) {
        if let Some(RObj::Array(slot)) = self.obj_mut(v) {
            *slot = xs;
        }
    }
    pub fn as_str(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::Str(s)) => Some(s.clone()),
            _ => match v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            },
        }
    }
    pub fn set_str(&mut self, v: &Value, s: String) {
        if let Some(RObj::Str(slot)) = self.obj_mut(v) {
            *slot = s;
        }
    }
    pub fn as_hash(&self, v: &Value) -> Option<IndexMap<RKey, Value>> {
        match self.obj(v) {
            Some(RObj::Hash(m)) => Some(m.clone()),
            _ => None,
        }
    }
    pub fn set_hash(&mut self, v: &Value, m: IndexMap<RKey, Value>) {
        if let Some(RObj::Hash(slot)) = self.obj_mut(v) {
            *slot = m;
        }
    }
    pub fn as_range(&self, v: &Value) -> Option<(i64, i64, bool)> {
        match self.obj(v) {
            Some(RObj::Range { lo, hi, exclusive }) => Some((*lo, *hi, *exclusive)),
            _ => None,
        }
    }
    pub fn as_symbol(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::Symbol(s)) => Some(s.clone()),
            _ => None,
        }
    }
    pub fn is_proc(&self, v: &Value) -> bool {
        matches!(self.obj(v), Some(RObj::Proc { .. }))
    }
    pub fn has_method(&self, name: &str) -> bool {
        self.methods.contains_key(name)
    }
    /// Whether a bare name resolves as a callable method — a class method (or
    /// `new`) when `self` is a class ref, an instance method on `self`'s class,
    /// or a top-level method.
    pub fn responds_to(&self, name: &str) -> bool {
        let this = self.current_self();
        if let Some(cls) = self.classref_name(&this) {
            return name == "new" || self.find_class_method(&cls, name).is_some();
        }
        if let Some(cls) = self.object_class(&this) {
            if self.find_method(&cls, name).is_some() {
                return true;
            }
        }
        self.methods.contains_key(name)
    }
    /// The class name of any value — the dynamic class for a user object, the
    /// builtin class name otherwise.
    pub fn class_of(&self, v: &Value) -> String {
        match self.obj(v) {
            Some(RObj::Object { class, .. }) => class.clone(),
            Some(RObj::ClassRef(_)) => "Class".to_string(),
            _ => self.class_name(v).to_string(),
        }
    }
    pub fn value_to_key(&self, v: &Value) -> RKey {
        self.to_key(v)
    }
    pub fn key_value(&mut self, k: &RKey) -> Value {
        self.key_to_value(k)
    }

    fn intern(&mut self, name: &str) -> Value {
        if let Some(id) = self.symbols.get(name) {
            return Value::Obj(*id);
        }
        let v = self.alloc(RObj::Symbol(name.to_string()));
        if let Value::Obj(id) = v {
            self.symbols.insert(name.to_string(), id);
        }
        v
    }

    // ---- variable / scope -------------------------------------------------

    /// The index of the frame local variable access should target.
    fn active_idx(&self) -> usize {
        self.active_frame.unwrap_or(self.frames.len() - 1)
    }
    fn frame(&mut self) -> &mut Frame {
        let i = self.active_idx();
        &mut self.frames[i]
    }
    pub fn get_local(&mut self, name: &str) -> Value {
        let i = self.active_idx();
        self.frames[i]
            .locals
            .get(name)
            .cloned()
            .unwrap_or(Value::Undef)
    }
    pub fn set_local(&mut self, name: &str, v: Value) {
        self.frame().locals.insert(name.to_string(), v);
    }
    pub fn local_defined(&self, name: &str) -> bool {
        self.frames[self.active_idx()].locals.contains_key(name)
    }
    pub fn get_global(&self, name: &str) -> Value {
        self.globals.get(name).cloned().unwrap_or(Value::Undef)
    }
    pub fn set_global(&mut self, name: &str, v: Value) {
        self.globals.insert(name.to_string(), v);
    }
    pub fn get_const(&self, name: &str) -> Value {
        self.consts.get(name).cloned().unwrap_or(Value::Undef)
    }
    pub fn set_const(&mut self, name: &str, v: Value) {
        self.consts.insert(name.to_string(), v);
    }
    // Instance vars live on the current `self` object; at the top level (self is
    // the main object) they fall back to a global-keyed table.
    pub fn get_ivar(&self, name: &str) -> Value {
        match self.current_self() {
            Value::Obj(_) => {
                if let Some(RObj::Object { ivars, .. }) = self.obj(&self.current_self()) {
                    return ivars.get(name).cloned().unwrap_or(Value::Undef);
                }
                Value::Undef
            }
            _ => self
                .globals
                .get(&format!("@{name}"))
                .cloned()
                .unwrap_or(Value::Undef),
        }
    }
    pub fn set_ivar(&mut self, name: &str, v: Value) {
        let this = self.current_self();
        match this {
            Value::Obj(i) => {
                if let Some(RObj::Object { ivars, .. }) = self.heap.get_mut(i as usize) {
                    ivars.insert(name.to_string(), v);
                }
            }
            _ => {
                self.globals.insert(format!("@{name}"), v);
            }
        }
    }

    // ---- classes / objects / self -----------------------------------------

    /// The receiver of the currently-active frame.
    pub fn current_self(&self) -> Value {
        self.frames[self.active_idx()].self_obj.clone()
    }
    /// Register a user class.
    pub fn add_class(&mut self, name: String, def: ClassDef) {
        self.classes.insert(name, def);
    }
    pub fn class_exists(&self, name: &str) -> bool {
        self.classes.contains_key(name)
    }
    /// Allocate an instance of `class`.
    pub fn new_object(&mut self, class: &str) -> Value {
        self.alloc(RObj::Object {
            class: class.to_string(),
            ivars: IndexMap::new(),
        })
    }
    pub fn class_ref(&mut self, name: &str) -> Value {
        self.alloc(RObj::ClassRef(name.to_string()))
    }
    /// The class name of a user object, if `v` is one.
    pub fn object_class(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::Object { class, .. }) => Some(class.clone()),
            _ => None,
        }
    }
    /// Whether `class` is an ancestor of (or equal to) `start` — walking the
    /// superclass chain and included modules.
    fn class_is_ancestor(&self, start: &str, class: &str) -> bool {
        let mut cur = Some(start.to_string());
        while let Some(name) = cur {
            if name == class {
                return true;
            }
            let Some(def) = self.classes.get(&name) else {
                break;
            };
            if def.includes.iter().any(|m| m == class) {
                return true;
            }
            cur = def.superclass.clone();
        }
        false
    }
    /// Ruby `is_a?` / `Class === obj`: does `v` belong to `class` (builtin type,
    /// `Numeric`/`Object` super-types, or a user class/module ancestor)?
    pub fn is_a(&self, v: &Value, class: &str) -> bool {
        let actual = self.class_of(v);
        if actual == class || class == "Object" || class == "BasicObject" {
            return true;
        }
        if class == "Numeric" && (actual == "Integer" || actual == "Float") {
            return true;
        }
        if class == "Comparable" && matches!(actual.as_str(), "Integer" | "Float" | "String") {
            return true;
        }
        if class == "Enumerable" && matches!(actual.as_str(), "Array" | "Hash" | "Range") {
            return true;
        }
        if let Some(oc) = self.object_class(v) {
            return self.class_is_ancestor(&oc, class);
        }
        false
    }
    /// Whether `name` is a builtin class/type name (for constant resolution).
    pub fn is_builtin_class(&self, name: &str) -> bool {
        matches!(
            name,
            "Integer"
                | "Float"
                | "Numeric"
                | "String"
                | "Symbol"
                | "Array"
                | "Hash"
                | "Range"
                | "Proc"
                | "Object"
                | "BasicObject"
                | "Comparable"
                | "Enumerable"
                | "NilClass"
                | "TrueClass"
                | "FalseClass"
        )
    }
    pub fn classref_name(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::ClassRef(n)) => Some(n.clone()),
            _ => None,
        }
    }
    /// Look up `method` on `class`, walking the ancestor chain (own methods,
    /// then included modules, then the superclass), returning the method and the
    /// class/module it was defined in.
    pub fn find_method_owner(&self, class: &str, method: &str) -> Option<(MethodDef, String)> {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let def = self.classes.get(&name)?;
            if let Some(m) = def.methods.get(method) {
                return Some((m.clone(), name.clone()));
            }
            // Included modules take priority over the superclass (last include
            // wins, matching Ruby's reverse-order ancestor insertion).
            for module in def.includes.iter().rev() {
                if let Some(md) = self.classes.get(module) {
                    if let Some(m) = md.methods.get(method) {
                        return Some((m.clone(), module.clone()));
                    }
                }
            }
            cur = def.superclass.clone();
        }
        None
    }
    /// Look up `method` on `class`, walking the ancestor chain.
    pub fn find_method(&self, class: &str, method: &str) -> Option<MethodDef> {
        self.find_method_owner(class, method).map(|(m, _)| m)
    }
    /// Resolve a `super` call: find `method` in the ancestors *above* `def_class`
    /// (its superclass chain), returning the method and its owner.
    pub fn find_super(&self, def_class: &str, method: &str) -> Option<(MethodDef, String)> {
        let sup = self.classes.get(def_class)?.superclass.clone()?;
        self.find_method_owner(&sup, method)
    }
    /// A class method (`def self.m`), walking the superclass chain.
    pub fn find_class_method(&self, class: &str, method: &str) -> Option<MethodDef> {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let def = self.classes.get(&name)?;
            if let Some(m) = def.class_methods.get(method) {
                return Some(m.clone());
            }
            cur = def.superclass.clone();
        }
        None
    }
    /// If `self_obj` is a user object whose class defines `method`, return the
    /// method, the owner class, and the receiver (for implicit-self calls).
    fn method_for_self(
        &self,
        self_obj: &Value,
        method: &str,
    ) -> Option<(MethodDef, String, Value)> {
        let class = self.object_class(self_obj)?;
        self.find_method_owner(&class, method)
            .map(|(m, owner)| (m, owner, self_obj.clone()))
    }
    pub fn ivar_of(&self, obj: &Value, name: &str) -> Value {
        match self.obj(obj) {
            Some(RObj::Object { ivars, .. }) => ivars.get(name).cloned().unwrap_or(Value::Undef),
            _ => Value::Undef,
        }
    }
    /// Bind method parameters to the call arguments, honoring a single `*splat`
    /// parameter (params before it bind positionally, the splat collects the
    /// middle into an array, params after it bind from the tail). Omitted
    /// non-splat params are left unbound so the method prologue applies defaults.
    pub fn bind_params(
        &mut self,
        params: &[String],
        splat: Option<usize>,
        kwparams: &[String],
        kwsplat: Option<&str>,
        args: &[Value],
    ) -> IndexMap<String, Value> {
        // With keyword params (explicit or a `**` collector), the final positional
        // argument (if it is a Hash) is the keyword hash; bind the rest
        // positionally.
        let wants_kw = !kwparams.is_empty() || kwsplat.is_some();
        let (positional, kwhash): (&[Value], Option<IndexMap<RKey, Value>>) = if wants_kw {
            match args.last() {
                Some(v) if matches!(self.obj(v), Some(RObj::Hash(_))) => {
                    (&args[..args.len() - 1], self.as_hash(v))
                }
                _ => (args, None),
            }
        } else {
            (args, None)
        };

        let mut locals = IndexMap::new();
        match splat {
            None => {
                for (i, p) in params.iter().enumerate() {
                    if let Some(v) = positional.get(i) {
                        locals.insert(p.clone(), v.clone());
                    }
                }
            }
            Some(si) => {
                let after = params.len() - si - 1;
                for (i, p) in params.iter().take(si).enumerate() {
                    if let Some(v) = positional.get(i) {
                        locals.insert(p.clone(), v.clone());
                    }
                }
                let splat_end = positional.len().saturating_sub(after).max(si);
                let rest: Vec<Value> = positional
                    .get(si..splat_end)
                    .map(|s| s.to_vec())
                    .unwrap_or_default();
                let arr = self.new_array(rest);
                locals.insert(params[si].clone(), arr);
                for (j, p) in params.iter().skip(si + 1).enumerate() {
                    if let Some(v) = positional.get(splat_end + j) {
                        locals.insert(p.clone(), v.clone());
                    }
                }
            }
        }
        // Bind keyword params from the keyword hash; omitted ones stay unbound so
        // the method prologue can apply their default (a required keyword left
        // unbound reads as nil).
        for kw in kwparams {
            let key = RKey::Sym(kw.clone());
            if let Some(v) = kwhash.as_ref().and_then(|m| m.get(&key)) {
                locals.insert(kw.clone(), v.clone());
            }
        }
        // A `**opts` collector receives the keyword entries not claimed by an
        // explicit keyword parameter.
        if let Some(name) = kwsplat {
            let mut rest = IndexMap::new();
            if let Some(m) = &kwhash {
                for (k, v) in m {
                    let claimed = matches!(k, RKey::Sym(s) if kwparams.iter().any(|p| p == s));
                    if !claimed {
                        rest.insert(k.clone(), v.clone());
                    }
                }
            }
            let h = self.new_hash(rest);
            locals.insert(name.to_string(), h);
        }
        locals
    }

    /// The `self`, method name, and defining class of the current frame (`super`).
    pub fn super_context(&self) -> (Value, Option<String>, Option<String>, Vec<Value>) {
        let f = &self.frames[self.active_idx()];
        (
            f.self_obj.clone(),
            f.method_name.clone(),
            f.def_class.clone(),
            f.args.clone(),
        )
    }

    // ---- exceptions -------------------------------------------------------

    pub fn set_pending_exc(&mut self, v: Value) {
        self.pending_exc = Some(v);
    }
    pub fn take_pending_exc(&mut self) -> Option<Value> {
        self.pending_exc.take()
    }
    /// Build an exception object of `class` carrying `message`.
    pub fn new_exception(&mut self, class: &str, message: &str) -> Value {
        let msg = self.new_string(message.to_string());
        let mut ivars = IndexMap::new();
        ivars.insert("message".to_string(), msg);
        self.alloc(RObj::Object {
            class: class.to_string(),
            ivars,
        })
    }
    /// Whether an exception of class `exc_class` is caught by a `rescue` naming
    /// `rescued` (walks the exception's superclass chain; unknown classes match
    /// generously so a bare `StandardError` rescue still fires).
    pub fn exc_matches(&self, exc_class: &str, rescued: &str) -> bool {
        if exc_class == rescued || rescued == "Exception" || rescued == "StandardError" {
            return true;
        }
        // Walk the user superclass chain if the exception is a user class.
        let mut cur = Some(exc_class.to_string());
        while let Some(name) = cur {
            if name == rescued {
                return true;
            }
            cur = self.classes.get(&name).and_then(|d| d.superclass.clone());
        }
        false
    }
    pub fn begin_def(&self, id: usize) -> Option<BeginDef> {
        self.begins.get(id).cloned()
    }
    pub fn proc_def(&self, id: usize) -> ProcDef {
        self.procs[id].clone()
    }

    // ---- truthiness / conversion -----------------------------------------

    /// Ruby truth: everything is true except `nil` and `false`.
    pub fn truthy(&self, v: &Value) -> bool {
        !matches!(v, Value::Undef | Value::Bool(false))
    }

    /// `to_s` — the human string form used by `puts`/interpolation.
    pub fn to_s(&mut self, v: &Value) -> String {
        match v {
            Value::Undef => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => fmt_float(*f),
            Value::Str(s) => s.to_string(),
            Value::Obj(_) => match self.obj(v).cloned() {
                Some(RObj::Str(s)) => s,
                Some(RObj::Symbol(s)) => s,
                Some(RObj::Range { lo, hi, exclusive }) => {
                    format!("{lo}{}{hi}", if exclusive { "..." } else { ".." })
                }
                Some(RObj::Array(items)) => self.inspect_array(&items),
                Some(RObj::Hash(map)) => self.inspect_hash(&map),
                Some(RObj::Proc { .. }) => "#<Proc>".to_string(),
                Some(RObj::ClassRef(n)) => n,
                Some(RObj::Object { class, ivars }) => {
                    // An exception object prints its message; other objects show
                    // their class, like Ruby's default `to_s`.
                    match ivars.get("message") {
                        Some(m) => self.to_s(&m.clone()),
                        None => format!("#<{class}>"),
                    }
                }
                None => "nil".to_string(),
            },
            _ => String::new(),
        }
    }

    /// `inspect` — the debug form used by `p`/`inspect` (quotes strings).
    pub fn inspect(&mut self, v: &Value) -> String {
        match v {
            Value::Undef => "nil".to_string(),
            Value::Str(s) => format!("{s:?}"),
            Value::Obj(_) => match self.obj(v).cloned() {
                Some(RObj::Str(s)) => format!("{s:?}"),
                Some(RObj::Symbol(s)) => format!(":{s}"),
                Some(RObj::Array(items)) => self.inspect_array(&items),
                Some(RObj::Hash(map)) => self.inspect_hash(&map),
                _ => self.to_s(v),
            },
            _ => self.to_s(v),
        }
    }

    fn inspect_array(&mut self, items: &[Value]) -> String {
        let parts: Vec<String> = items.iter().map(|it| self.inspect(it)).collect();
        format!("[{}]", parts.join(", "))
    }
    fn inspect_hash(&mut self, map: &IndexMap<RKey, Value>) -> String {
        let parts: Vec<String> = map
            .iter()
            .map(|(k, v)| {
                let vs = self.inspect(v);
                // Ruby 3.4+ prints a symbol key as `name: value`; every other
                // key type keeps the `key => value` form.
                match k {
                    RKey::Sym(s) => format!("{s}: {vs}"),
                    _ => format!("{} => {vs}", self.key_inspect(k)),
                }
            })
            .collect();
        format!("{{{}}}", parts.join(", "))
    }
    fn key_inspect(&mut self, k: &RKey) -> String {
        match k {
            RKey::Int(n) => n.to_string(),
            RKey::Str(s) => format!("{s:?}"),
            RKey::Sym(s) => format!(":{s}"),
            RKey::Bool(b) => b.to_string(),
            RKey::Nil => "nil".to_string(),
            RKey::FloatBits(b) => fmt_float(f64::from_bits(*b)),
        }
    }

    fn class_name(&self, v: &Value) -> &'static str {
        match v {
            Value::Undef => "NilClass",
            Value::Bool(true) => "TrueClass",
            Value::Bool(false) => "FalseClass",
            Value::Int(_) => "Integer",
            Value::Float(_) => "Float",
            Value::Str(_) => "String",
            Value::Obj(_) => match self.obj(v) {
                Some(RObj::Str(_)) => "String",
                Some(RObj::Array(_)) => "Array",
                Some(RObj::Hash(_)) => "Hash",
                Some(RObj::Symbol(_)) => "Symbol",
                Some(RObj::Range { .. }) => "Range",
                Some(RObj::Proc { .. }) => "Proc",
                Some(RObj::ClassRef(_)) => "Class",
                Some(RObj::Object { .. }) => "Object",
                None => "Object",
            },
            _ => "Object",
        }
    }

    fn to_key(&self, v: &Value) -> RKey {
        match v {
            Value::Int(n) => RKey::Int(*n),
            Value::Float(f) => RKey::FloatBits(f.to_bits()),
            Value::Bool(b) => RKey::Bool(*b),
            Value::Undef => RKey::Nil,
            Value::Str(s) => RKey::Str(s.to_string()),
            Value::Obj(_) => match self.obj(v) {
                Some(RObj::Str(s)) => RKey::Str(s.clone()),
                Some(RObj::Symbol(s)) => RKey::Sym(s.clone()),
                _ => RKey::Str(format!("{v:?}")),
            },
            _ => RKey::Nil,
        }
    }
    fn key_to_value(&mut self, k: &RKey) -> Value {
        match k {
            RKey::Int(n) => Value::Int(*n),
            RKey::Str(s) => self.new_string(s.clone()),
            RKey::Sym(s) => self.intern(s),
            RKey::Bool(b) => Value::Bool(*b),
            RKey::Nil => Value::Undef,
            RKey::FloatBits(b) => Value::Float(f64::from_bits(*b)),
        }
    }

    // ---- numeric hook (Ruby semantics for non-native operands) ------------

    /// Called by fusevm when a native numeric op has a non-`Int`/`Float`
    /// operand: string/array `+`, string `*`, cross-type `==`, ordering.
    pub fn num_op(&mut self, op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
        use NumOp::*;
        // Equality is defined across every type pair.
        match op {
            Eq => return Ok(Value::Bool(self.eq_values(a, b))),
            Ne => return Ok(Value::Bool(!self.eq_values(a, b))),
            _ => {}
        }
        // String and Array operators.
        match (self.obj(a).cloned(), op) {
            (Some(RObj::Str(s)), Add) => {
                let bs = self.to_s(b);
                return Ok(self.new_string(format!("{s}{bs}")));
            }
            (Some(RObj::Str(s)), Mul) => {
                let n = as_int(b).unwrap_or(0).max(0) as usize;
                return Ok(self.new_string(s.repeat(n)));
            }
            (Some(RObj::Str(s)), Lt | Gt | Le | Ge) => {
                if let Some(RObj::Str(bs)) = self.obj(b) {
                    return Ok(Value::Bool(cmp_ord(op, s.cmp(bs))));
                }
            }
            (Some(RObj::Array(mut xs)), Add) => {
                if let Some(RObj::Array(ys)) = self.obj(b).cloned() {
                    xs.extend(ys);
                    return Ok(self.new_array(xs));
                }
            }
            (Some(RObj::Array(xs)), Mul) => {
                let n = as_int(b).unwrap_or(0).max(0) as usize;
                let mut out = Vec::with_capacity(xs.len() * n);
                for _ in 0..n {
                    out.extend(xs.iter().cloned());
                }
                return Ok(self.new_array(out));
            }
            _ => {}
        }
        Err(format!(
            "undefined method '{}' for {}",
            num_op_name(op),
            self.class_name(a)
        ))
    }

    /// Structural equality (`==`).
    pub fn eq_values(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => *x as f64 == *y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Undef, Value::Undef) => true,
            _ => {
                let (oa, ob) = (self.obj(a), self.obj(b));
                match (oa, ob) {
                    (Some(RObj::Str(x)), Some(RObj::Str(y))) => x == y,
                    (Some(RObj::Symbol(x)), Some(RObj::Symbol(y))) => x == y,
                    (Some(RObj::Array(x)), Some(RObj::Array(y))) => {
                        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.eq_values(p, q))
                    }
                    _ => matches!((a, b), (Value::Obj(i), Value::Obj(j)) if i == j),
                }
            }
        }
    }
}

/// Format an `f64` the way Ruby prints a Float (always shows a decimal point).
fn fmt_float(f: f64) -> String {
    if f == f.trunc() && f.is_finite() && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn cmp_ord(op: NumOp, o: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        NumOp::Lt => o == Less,
        NumOp::Gt => o == Greater,
        NumOp::Le => o != Greater,
        NumOp::Ge => o != Less,
        _ => false,
    }
}

fn num_op_name(op: NumOp) -> &'static str {
    match op {
        NumOp::Add => "+",
        NumOp::Sub => "-",
        NumOp::Mul => "*",
        NumOp::Div => "/",
        NumOp::Mod => "%",
        NumOp::Pow => "**",
        NumOp::Lt => "<",
        NumOp::Gt => ">",
        NumOp::Le => "<=",
        NumOp::Ge => ">=",
        _ => "<op>",
    }
}

// ===========================================================================
// Running chunks: method calls, block invocation, top-level program.
// ===========================================================================

/// Register every rubylang builtin + the numeric hook on a VM, then run it.
fn run_chunk_on(chunk: Chunk) -> Result<Value, String> {
    let mut vm = VM::new(chunk);
    crate::builtins::install(&mut vm);
    vm.set_numeric_hook(std::sync::Arc::new(|op, a, b| {
        crate::builtins::numeric_hook(op, a, b)
    }));
    vm.enable_tracing_jit();
    let outcome = vm.run();
    if let Some(e) = with_host(|h| h.take_error()) {
        return Err(e);
    }
    match outcome {
        VMResult::Ok(v) => Ok(v),
        VMResult::Halted => Ok(vm.stack.last().cloned().unwrap_or(Value::Undef)),
        VMResult::Error(e) => Err(e),
    }
}

/// Run the top-level program chunk. Clears any leftover control signal (a
/// top-level `return` just halts the program).
pub fn run_main(chunk: Chunk) -> Result<Value, String> {
    let r = run_chunk_on(chunk);
    with_host(|h| h.signal = None);
    r
}

/// Run a resolved method: push a fresh frame bound to `self_obj`, bind args, and
/// run the body with locals targeting that new top frame.
#[allow(clippy::too_many_arguments)]
fn run_method(
    def: &MethodDef,
    self_obj: Value,
    args: &[Value],
    block: Option<Value>,
    method_name: Option<String>,
    def_class: Option<String>,
) -> Result<Value, String> {
    let saved_active = with_host(|h| {
        let locals = h.bind_params(
            &def.params,
            def.splat,
            &def.kwparams,
            def.kwsplat.as_deref(),
            args,
        );
        h.frames.push(Frame {
            locals,
            block,
            self_obj,
            method_name,
            def_class,
            args: args.to_vec(),
        });
        h.active_frame.take()
    });
    let r = run_chunk_on(def.chunk.clone());
    let sig = with_host(|h| {
        h.frames.pop();
        h.active_frame = saved_active;
        h.signal.take()
    });
    match sig {
        Some(Signal::Return(v)) => Ok(v),
        _ => r,
    }
}

/// Invoke a top-level / implicit-self method by name. If the current `self` is a
/// user object, its class methods take priority (an unqualified call inside an
/// instance method dispatches on `self`).
pub fn call_method(name: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let self_obj = with_host(|h| h.current_self());
    if let Some((def, owner, recv)) = with_host(|h| h.method_for_self(&self_obj, name)) {
        return run_method(&def, recv, args, block, Some(name.into()), Some(owner));
    }
    let def = with_host(|h| h.methods.get(name).cloned());
    let Some(def) = def else {
        return Err(format!("undefined method '{name}'"));
    };
    run_method(&def, self_obj, args, block, Some(name.into()), None)
}

/// Invoke an instance method `name` on `recv` (an object of class `class`),
/// resolving it through the ancestor chain.
pub fn call_instance_method(
    recv: Value,
    class: &str,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let (def, owner) = with_host(|h| h.find_method_owner(class, name))
        .ok_or_else(|| format!("undefined method '{name}' for {class}"))?;
    run_method(&def, recv, args, block, Some(name.into()), Some(owner))
}

/// Invoke a class method (`def self.m`) with `self` bound to the class ref.
pub fn call_class_method(
    recv: Value,
    def: &MethodDef,
    name: &str,
    def_class: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    run_method(
        def,
        recv,
        args,
        block,
        Some(name.into()),
        Some(def_class.into()),
    )
}

/// Invoke `super`: resume the method lookup above the current frame's defining
/// class. `args` is `None` to forward the current method's arguments.
pub fn call_super(explicit_args: Option<Vec<Value>>) -> Result<Value, String> {
    let (self_obj, method, def_class, cur_args) = with_host(|h| h.super_context());
    let (Some(method), Some(def_class)) = (method, def_class) else {
        return Err("super called outside of a method".to_string());
    };
    let Some((def, owner)) = with_host(|h| h.find_super(&def_class, &method)) else {
        return Err(format!("super: no superclass method '{method}'"));
    };
    let args = explicit_args.unwrap_or(cur_args);
    let block = with_host(|h| h.frames[h.active_idx()].block.clone());
    run_method(&def, self_obj, &args, block, Some(method), Some(owner))
}

/// Run a proc *template* (by id) in the current frame — used for `begin`/`rescue`
/// /`ensure` bodies, which do not open a new scope. Params (the `rescue => e`
/// binding) are bound into the current frame and restored afterward.
fn run_template(id: usize, args: &[Value]) -> Result<Value, String> {
    let def = with_host(|h| h.proc_def(id));
    let saved: Vec<(String, Option<Value>)> = with_host(|h| {
        def.params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let prev = h.get_local(p);
                let had = h.local_defined(p);
                h.set_local(p, args.get(i).cloned().unwrap_or(Value::Undef));
                (p.clone(), had.then_some(prev))
            })
            .collect()
    });
    let r = run_chunk_on(def.chunk.clone());
    with_host(|h| {
        for (p, prev) in saved {
            match prev {
                Some(v) => h.set_local(&p, v),
                None => {
                    let i = h.active_idx();
                    h.frames[i].locals.shift_remove(&p);
                }
            }
        }
    });
    r
}

/// Run a `begin`/`rescue`/`ensure` block. The body runs; a raised exception is
/// matched against each `rescue` clause (by class); `ensure` always runs. An
/// unrescued exception is re-raised so an outer `begin` (or the top level) sees
/// it.
pub fn run_begin(begin_id: usize) -> Result<Value, String> {
    let Some(bd) = with_host(|h| h.begin_def(begin_id)) else {
        return Err("bad begin id".to_string());
    };

    let mut result = run_template(bd.body, &[]);

    let err = result.as_ref().err().cloned();
    if let Some(e) = err {
        // Only a *raised exception* (pending_exc set) is rescuable; a bare
        // `return`/`break` signal must fall through untouched.
        let has_signal = with_host(|h| h.signal.is_some());
        if !has_signal {
            let exc = with_host(|h| h.take_pending_exc());
            let exc_class = exc
                .as_ref()
                .and_then(|v| with_host(|h| h.object_class(v)))
                .unwrap_or_else(|| "StandardError".to_string());
            let excv = exc.clone().unwrap_or(Value::Undef);
            let mut handled = false;
            for rd in &bd.rescues {
                let matches = rd.classes.is_empty()
                    || rd
                        .classes
                        .iter()
                        .any(|c| with_host(|h| h.exc_matches(&exc_class, c)));
                if matches {
                    let args = if rd.binding.is_some() {
                        vec![excv.clone()]
                    } else {
                        vec![]
                    };
                    result = run_template(rd.body, &args);
                    handled = true;
                    break;
                }
            }
            if !handled {
                // Re-raise for an outer handler.
                with_host(|h| h.pending_exc = exc);
                result = Err(e);
            }
        }
    }

    if let Some(eid) = bd.ensure {
        // `ensure` always runs; an exception it raises supersedes the result.
        run_template(eid, &[])?;
    }
    result
}

/// Invoke a block/proc with the given arguments in the frame it was *created*
/// in (Ruby blocks capture and mutate the lexical surrounding scope). Block
/// params are bound for the duration and restored afterward. A single Array
/// argument to a multi-parameter block is destructured, matching Ruby.
pub fn call_proc(proc_val: &Value, args: &[Value]) -> Result<Value, String> {
    let (template, frame) = match with_host(|h| h.obj(proc_val).cloned()) {
        Some(RObj::Proc { template, frame }) => (template, frame),
        _ => return Err("not a proc".to_string()),
    };
    let def = with_host(|h| h.procs[template].clone());

    // Auto-splat: `pairs.each { |k, v| … }` over `[k, v]` elements.
    let bound: Vec<Value> = if def.params.len() > 1 && args.len() == 1 {
        match with_host(|h| h.as_array(&args[0])) {
            Some(items) => items,
            None => args.to_vec(),
        }
    } else {
        args.to_vec()
    };

    // Run in the captured frame; save/restore both the active-frame pointer and
    // any locals the block params shadow.
    let prev_active = with_host(|h| h.active_frame.replace(frame));
    let saved: Vec<(String, Option<Value>)> = with_host(|h| {
        def.params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let prev = h.frames[frame].locals.get(p).cloned();
                let v = bound.get(i).cloned().unwrap_or(Value::Undef);
                h.set_local(p, v);
                (p.clone(), prev)
            })
            .collect()
    });
    let r = run_chunk_on(def.chunk.clone());
    with_host(|h| {
        for (p, prev) in saved {
            match prev {
                Some(v) => {
                    h.frames[frame].locals.insert(p, v);
                }
                None => {
                    h.frames[frame].locals.shift_remove(&p);
                }
            }
        }
        h.active_frame = prev_active;
    });
    // A `next` inside the block becomes the block's value; break/return propagate.
    let sig = with_host(|h| h.signal.take());
    match sig {
        Some(Signal::Next(v)) => Ok(v),
        Some(other) => {
            with_host(|h| h.signal = Some(other));
            r
        }
        None => r,
    }
}

/// The block passed to the current method (for `yield`).
pub fn current_block() -> Option<Value> {
    with_host(|h| {
        let i = h.active_idx();
        h.frames[i].block.clone()
    })
}

/// Set a control signal (break/next/return) — checked by the frame/loop above.
pub fn raise_signal_break(v: Value) {
    with_host(|h| h.signal = Some(Signal::Break(v)));
}
pub fn raise_signal_next(v: Value) {
    with_host(|h| h.signal = Some(Signal::Next(v)));
}
pub fn raise_signal_return(v: Value) {
    with_host(|h| h.signal = Some(Signal::Return(v)));
}
pub fn take_break() -> Option<Value> {
    with_host(|h| match &h.signal {
        Some(Signal::Break(_)) => {
            if let Some(Signal::Break(v)) = h.signal.take() {
                Some(v)
            } else {
                None
            }
        }
        _ => None,
    })
}
pub fn has_pending_signal() -> bool {
    with_host(|h| h.signal.is_some())
}
