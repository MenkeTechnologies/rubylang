//! The Ruby object heap and runtime, reached from fusevm through registered
//! builtins (`register_builtin`) and the strict numeric hook.
//!
//! rubylang owns no VM and no JIT: the compiler lowers Ruby to `fusevm::Chunk`,
//! and every Ruby-specific operation the VM can't do natively is a builtin call
//! that lands here. Local variables live in `Rc<RefCell>` environments chained
//! parent-to-child, so a block/lambda captures its defining scope by reference —
//! keeping those variables alive and shared after the method returns (real Ruby
//! closure semantics), while block params stay block-local.
//!
//! Value representation:
//!   - immediate: `Value::Int` (Integer), `Value::Float` (Float),
//!     `Value::Bool` (true/false), `Value::Undef` (nil);
//!   - heap `Value::Obj(u32)` handles: String, Array, Hash, Symbol, Range, Proc
//!     — the reference types, so `a.push(x)` mutates in place like real Ruby.

use fusevm::{Chunk, NumOp, VMResult, Value, VM};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

/// A local-variable environment, shared (by `Rc`) between a frame and any block
/// or lambda that captures it — so a closure keeps its variables alive after the
/// defining method returns, and closure/enclosing mutations are mutually visible.
/// A block gets its own env whose `parent` is the captured one, so block params
/// are block-local while enclosing variables remain read/writable (Ruby's scope
/// chain). Method frames have no parent (a fresh scope).
pub struct EnvData {
    vars: IndexMap<String, Value>,
    parent: Option<Env>,
}
pub type Env = Rc<RefCell<EnvData>>;

fn new_env() -> Env {
    Rc::new(RefCell::new(EnvData {
        vars: IndexMap::new(),
        parent: None,
    }))
}
fn env_with(vars: IndexMap<String, Value>) -> Env {
    Rc::new(RefCell::new(EnvData { vars, parent: None }))
}
fn child_env(parent: Env) -> Env {
    Rc::new(RefCell::new(EnvData {
        vars: IndexMap::new(),
        parent: Some(parent),
    }))
}

/// The lexical + dynamic context a block/lambda captures: its variable
/// environment plus the `self`, block, and method identity in effect where it
/// was written.
#[derive(Clone)]
pub struct Scope {
    locals: Env,
    self_obj: Value,
    block: Option<Value>,
    method_name: Option<String>,
    def_class: Option<String>,
}

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<scope>")
    }
}

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
    pub const MKREGEX: u16 = 35; // [source, flags] -> Regexp
    pub const MKLAMBDA: u16 = 36; // [proc_id] -> Proc (lambda? == true)
    pub const SIG_RETRY: u16 = 37; // [v] -> restart the enclosing begin body
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
    /// A block/proc/lambda: its compiled template plus the captured lexical
    /// scope (Ruby blocks read and write the variables of the scope where they
    /// appear, even after that method has returned). `is_lambda` distinguishes a
    /// `->`/`lambda` proc (strict arity, `return` is local) from a plain block.
    /// `kind` carries the derived-proc state produced by `curry`/`>>`/`<<`.
    Proc {
        template: usize,
        scope: Scope,
        is_lambda: bool,
        kind: ProcKind,
    },
    /// A native proc produced by `Symbol#to_proc` (`&:upcase`): calling it sends
    /// the named method to its first argument (`:upcase.to_proc.call(s)` == `s.upcase`).
    SymProc(String),
    /// A user-defined object: its class name and its instance variables.
    Object {
        class: String,
        ivars: IndexMap<String, Value>,
    },
    /// A reference to a class/module (the value of a constant like `Foo`), used
    /// as the receiver of `Foo.new`, `Foo.name`, etc.
    ClassRef(String),
    /// A compiled regular expression: its Ruby source plus the compiled matcher.
    Regexp {
        source: String,
        re: regex::Regex,
    },
    /// The result of a successful `String#match` / `Regexp#match`: the group
    /// captures (index 0 is the whole match; `None` = an unmatched optional
    /// group) plus the text before and after the whole match.
    MatchData {
        groups: Vec<Option<String>>,
        pre: String,
        post: String,
    },
}

/// How a `Proc` value behaves when called. `Normal` runs its own template;
/// `Curried`/`Composed` are the derived procs built by `Proc#curry`, `#>>` and
/// `#<<`. Ruby keeps these as ordinary `Proc` instances, so they still route to
/// the Proc dispatcher and report `class == Proc`.
#[derive(Debug, Clone)]
pub enum ProcKind {
    /// Runs the proc's own `template` in its captured `scope`.
    Normal,
    /// A partially-applied proc: it needs `arity` total args and has already
    /// gathered `collected`; when full it runs the base `template`/`scope`.
    Curried {
        arity: usize,
        collected: Vec<Value>,
    },
    /// Function composition: call `first`, feed its result to `second`.
    /// `f >> g` builds `{ first: f, second: g }`; `f << g` builds `{ first: g,
    /// second: f }`.
    Composed {
        first: Box<Value>,
        second: Box<Value>,
    },
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
    /// `&blk` block-capture parameter name, if any.
    pub blockparam: Option<String>,
    pub chunk: Chunk,
}

/// A compiled block template.
#[derive(Clone)]
pub struct ProcDef {
    pub params: Vec<String>,
    pub chunk: Chunk,
}

/// One method activation (or the top level): its captured scope plus the args it
/// was called with (for a bare `super`).
struct Frame {
    scope: Scope,
    args: Vec<Value>,
}

/// A non-local control signal raised by `break`/`next`/`return`/`retry`.
#[derive(Clone)]
enum Signal {
    Break(Value),
    Next(Value),
    Return(Value),
    /// `retry` inside a `rescue` clause — restarts the enclosing `begin` body.
    Retry,
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
    /// The scope local/`self`/block access targets. `None` = the top frame (a
    /// method body / top level); `Some(scope)` = a captured scope while a block
    /// or lambda that captured it is running.
    active_scope: Option<Scope>,
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
                scope: Scope {
                    locals: new_env(),
                    block: None,
                    self_obj: Value::Undef,
                    method_name: None,
                    def_class: None,
                },
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
            active_scope: None,
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
    /// Shallow copy of `v` for `Object#dup`/`clone`: reference types get a fresh
    /// heap object whose contents alias the original (like Ruby's shallow dup);
    /// immediates (Int/Float/Bool/nil/Symbol) return unchanged.
    pub fn dup_value(&mut self, v: &Value) -> Value {
        match self.obj(v) {
            // Interned symbols and class references dup to themselves in Ruby;
            // copying them would break identity/interning.
            Some(RObj::Symbol(_)) | Some(RObj::ClassRef(_)) => v.clone(),
            Some(obj) => {
                let copy = obj.clone();
                self.alloc(copy)
            }
            None => v.clone(),
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
    /// Compile a regex literal (Ruby `flags` → Rust inline flags: `i`
    /// case-insensitive, `m` dot-matches-newline, `x` extended). Returns an error
    /// string if the pattern is not valid for the Rust regex engine.
    pub fn new_regex(&mut self, source: &str, flags: &str) -> Result<Value, String> {
        let mut inline = String::new();
        if flags.contains('i') {
            inline.push('i');
        }
        if flags.contains('m') {
            inline.push('s'); // Ruby /m/ = dot matches newline = Rust (?s)
        }
        if flags.contains('x') {
            inline.push('x');
        }
        let full = if inline.is_empty() {
            source.to_string()
        } else {
            format!("(?{inline}){source}")
        };
        match regex::Regex::new(&full) {
            Ok(re) => Ok(self.alloc(RObj::Regexp {
                source: source.to_string(),
                re,
            })),
            Err(e) => Err(format!("invalid regex /{source}/: {e}")),
        }
    }
    /// The `(groups, pre, post)` of a `MatchData` value, if `v` is one.
    #[allow(clippy::type_complexity)]
    pub fn as_matchdata(&self, v: &Value) -> Option<(Vec<Option<String>>, String, String)> {
        match self.obj(v) {
            Some(RObj::MatchData { groups, pre, post }) => {
                Some((groups.clone(), pre.clone(), post.clone()))
            }
            _ => None,
        }
    }
    /// The compiled matcher + source of a regex value, if `v` is one.
    pub fn as_regex(&self, v: &Value) -> Option<(regex::Regex, String)> {
        match self.obj(v) {
            Some(RObj::Regexp { re, source }) => Some((re.clone(), source.clone())),
            _ => None,
        }
    }
    /// Build a `MatchData` for `re.captures(subject)` at the point where the whole
    /// match spans `[start, end)`.
    pub fn new_matchdata(
        &mut self,
        groups: Vec<Option<String>>,
        pre: String,
        post: String,
    ) -> Value {
        self.alloc(RObj::MatchData { groups, pre, post })
    }

    /// Create a proc capturing the currently-active scope (shared by `Rc`).
    pub fn new_proc(&mut self, template: usize) -> Value {
        let scope = self.cur_scope().clone();
        self.alloc(RObj::Proc {
            template,
            scope,
            is_lambda: false,
            kind: ProcKind::Normal,
        })
    }
    /// Create a lambda (same as `new_proc` but `lambda?` is `true`).
    pub fn new_lambda(&mut self, template: usize) -> Value {
        let scope = self.cur_scope().clone();
        self.alloc(RObj::Proc {
            template,
            scope,
            is_lambda: true,
            kind: ProcKind::Normal,
        })
    }
    /// `true` if this proc was made by `->`/`lambda` (not a plain block).
    pub fn proc_is_lambda(&self, v: &Value) -> bool {
        matches!(self.obj(v), Some(RObj::Proc { is_lambda: true, .. }))
    }
    /// Mark an existing proc as a lambda (used by the `lambda` Kernel method).
    pub fn set_proc_lambda(&mut self, v: &Value) {
        if let Some(RObj::Proc { is_lambda, .. }) = self.obj_mut(v) {
            *is_lambda = true;
        }
    }
    /// Ruby `Proc#arity`. A curried proc reports `-1`; a normal proc reports its
    /// declared parameter count (all params here are required, so arity is exact).
    pub fn proc_arity(&self, v: &Value) -> Option<i64> {
        match self.obj(v) {
            Some(RObj::Proc { kind, template, .. }) => match kind {
                ProcKind::Curried { .. } | ProcKind::Composed { .. } => Some(-1),
                ProcKind::Normal => Some(self.procs[*template].params.len() as i64),
            },
            _ => None,
        }
    }
    /// Build the curried view of a proc: shares the base template/scope but only
    /// runs once `arity` args are gathered across successive calls.
    pub fn proc_curry(&mut self, v: &Value) -> Option<Value> {
        match self.obj(v).cloned() {
            Some(RObj::Proc {
                template,
                scope,
                is_lambda,
                kind,
            }) => {
                let arity = match kind {
                    ProcKind::Curried { arity, .. } => arity,
                    ProcKind::Composed { .. } => return Some(v.clone()),
                    ProcKind::Normal => self.procs[template].params.len(),
                };
                Some(self.alloc(RObj::Proc {
                    template,
                    scope,
                    is_lambda,
                    kind: ProcKind::Curried {
                        arity,
                        collected: Vec::new(),
                    },
                }))
            }
            _ => None,
        }
    }
    /// Build a composed proc `first` then `second` (both are `Proc` values).
    pub fn new_composed(&mut self, first: Value, second: Value, is_lambda: bool) -> Value {
        let scope = self.cur_scope().clone();
        self.alloc(RObj::Proc {
            template: 0,
            scope,
            is_lambda,
            kind: ProcKind::Composed {
                first: Box::new(first),
                second: Box::new(second),
            },
        })
    }
    pub fn new_symbol(&mut self, name: &str) -> Value {
        self.intern(name)
    }
    /// Allocate the native proc backing `Symbol#to_proc`.
    pub fn new_sym_proc(&mut self, sym: &str) -> Value {
        self.alloc(RObj::SymProc(sym.to_string()))
    }
    /// The method name a `Symbol#to_proc` proc dispatches (`None` for a normal proc).
    pub fn as_sym_proc(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::SymProc(s)) => Some(s.clone()),
            _ => None,
        }
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
        matches!(self.obj(v), Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)))
    }
    pub fn has_method(&self, name: &str) -> bool {
        self.methods.contains_key(name)
    }
    /// Names defined live on this host — top-level methods, classes/modules,
    /// constants, and globals (`$name`). The REPL merges these with the static
    /// keyword/builtin corpus so a `def`/`class`/assignment made on a prior
    /// prompt completes on the next one. Class and const names overlap (a class
    /// is a const), so the result is de-duplicated by the caller.
    pub fn repl_completion_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        out.extend(self.methods.keys().cloned());
        out.extend(self.classes.keys().cloned());
        out.extend(self.consts.keys().cloned());
        // Globals carry their `$` sigil so they complete as `$name`.
        out.extend(self.globals.keys().map(|g| format!("${g}")));
        out
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

    /// The scope local/`self`/block access should target: a captured scope while
    /// a block runs, else the top method frame.
    fn cur_scope(&self) -> &Scope {
        self.active_scope
            .as_ref()
            .unwrap_or(&self.frames.last().unwrap().scope)
    }
    /// The active local-variable environment (shared, interior-mutable).
    fn cur_env(&self) -> Env {
        self.cur_scope().locals.clone()
    }
    /// Read a local, walking the scope chain to enclosing environments.
    pub fn get_local(&self, name: &str) -> Value {
        let mut env = self.cur_env();
        loop {
            if let Some(v) = env.borrow().vars.get(name).cloned() {
                return v;
            }
            let parent = env.borrow().parent.clone();
            match parent {
                Some(p) => env = p,
                None => return Value::Undef,
            }
        }
    }
    /// Assign a local: update it where it already exists in the chain (so a block
    /// mutates an enclosing variable), else create it in the innermost scope.
    pub fn set_local(&self, name: &str, v: Value) {
        let mut env = self.cur_env();
        loop {
            if env.borrow().vars.contains_key(name) {
                env.borrow_mut().vars.insert(name.to_string(), v);
                return;
            }
            let parent = env.borrow().parent.clone();
            match parent {
                Some(p) => env = p,
                None => break,
            }
        }
        self.cur_env().borrow_mut().vars.insert(name.to_string(), v);
    }
    pub fn local_defined(&self, name: &str) -> bool {
        let mut env = self.cur_env();
        loop {
            if env.borrow().vars.contains_key(name) {
                return true;
            }
            let parent = env.borrow().parent.clone();
            match parent {
                Some(p) => env = p,
                None => return false,
            }
        }
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
        self.cur_scope().self_obj.clone()
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
    /// The direct superclass of a user class, if registered.
    pub fn superclass_of(&self, name: &str) -> Option<String> {
        self.classes.get(name).and_then(|d| d.superclass.clone())
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
    /// Set the instance variable `name` (bare, no `@`) on a specific object.
    pub fn set_ivar_of(&mut self, obj: &Value, name: &str, v: Value) {
        if let Value::Obj(i) = obj {
            if let Some(RObj::Object { ivars, .. }) = self.heap.get_mut(*i as usize) {
                ivars.insert(name.to_string(), v);
            }
        }
    }
    /// The instance-variable names of `obj`, each with its `@` sigil restored.
    pub fn ivar_names(&self, obj: &Value) -> Vec<String> {
        match self.obj(obj) {
            Some(RObj::Object { ivars, .. }) => ivars.keys().map(|k| format!("@{k}")).collect(),
            _ => Vec::new(),
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
        let s = self.cur_scope();
        (
            s.self_obj.clone(),
            s.method_name.clone(),
            s.def_class.clone(),
            self.frames.last().unwrap().args.clone(),
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
                Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) => "#<Proc>".to_string(),
                Some(RObj::Regexp { source, .. }) => format!("(?-mix:{source})"),
                // MatchData#to_s is the whole matched substring (group 0).
                Some(RObj::MatchData { groups, .. }) => {
                    groups.first().and_then(|g| g.clone()).unwrap_or_default()
                }
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
                Some(RObj::Regexp { source, .. }) => format!("/{source}/"),
                // `#<MatchData "ll" 1:"l">` — whole match then numbered groups.
                Some(RObj::MatchData { groups, .. }) => {
                    let whole = groups.first().and_then(|g| g.clone()).unwrap_or_default();
                    let mut out = format!("#<MatchData {whole:?}");
                    for (i, g) in groups.iter().enumerate().skip(1) {
                        match g {
                            Some(s) => out.push_str(&format!(" {i}:{s:?}")),
                            None => out.push_str(&format!(" {i}:nil")),
                        }
                    }
                    out.push('>');
                    out
                }
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
                Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) => "Proc",
                Some(RObj::Regexp { .. }) => "Regexp",
                Some(RObj::MatchData { .. }) => "MatchData",
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
        let mut binding = h.bind_params(
            &def.params,
            def.splat,
            &def.kwparams,
            def.kwsplat.as_deref(),
            args,
        );
        // `&blk` captures the passed block as a Proc (or nil if none was given).
        if let Some(bp) = &def.blockparam {
            binding.insert(bp.clone(), block.clone().unwrap_or(Value::Undef));
        }
        h.frames.push(Frame {
            scope: Scope {
                locals: env_with(binding),
                block,
                self_obj,
                method_name,
                def_class,
            },
            args: args.to_vec(),
        });
        // A method body runs against its own top frame, not any captured block
        // scope in effect at the call site.
        h.active_scope.take()
    });
    let r = run_chunk_on(def.chunk.clone());
    let sig = with_host(|h| {
        h.frames.pop();
        h.active_scope = saved_active;
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
    let block = with_host(|h| h.cur_scope().block.clone());
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
        let env = h.cur_env();
        for (p, prev) in saved {
            match prev {
                Some(v) => {
                    env.borrow_mut().vars.insert(p, v);
                }
                None => {
                    env.borrow_mut().vars.shift_remove(&p);
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

    // The body may run more than once: a `retry` inside a matching `rescue`
    // clause restarts it from the top.
    let result = loop {
        let mut result = run_template(bd.body, &[]);

        let err = result.as_ref().err().cloned();
        let Some(e) = err else {
            break result;
        };
        // Only a *raised exception* (pending_exc set) is rescuable; a bare
        // `return`/`break` signal must fall through untouched.
        let has_signal = with_host(|h| h.signal.is_some());
        if has_signal {
            break result;
        }
        let exc = with_host(|h| h.take_pending_exc());
        let exc_class = exc
            .as_ref()
            .and_then(|v| with_host(|h| h.object_class(v)))
            .unwrap_or_else(|| "StandardError".to_string());
        let excv = exc.clone().unwrap_or(Value::Undef);
        let mut handled = false;
        let mut retrying = false;
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
                // A `retry` in the clause clears itself and restarts the body.
                retrying = take_retry_signal();
                break;
            }
        }
        if retrying {
            continue;
        }
        if !handled {
            // Re-raise for an outer handler.
            with_host(|h| h.pending_exc = exc);
            result = Err(e);
        }
        break result;
    };

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
    let (template, scope, kind) = match with_host(|h| h.obj(proc_val).cloned()) {
        Some(RObj::Proc {
            template,
            scope,
            kind,
            ..
        }) => (template, scope, kind),
        _ => return Err("not a proc".to_string()),
    };

    // Derived procs (curry / composition) delegate rather than run a template.
    match kind {
        ProcKind::Composed { first, second } => {
            let mid = call_proc(&first, args)?;
            return call_proc(&second, std::slice::from_ref(&mid));
        }
        ProcKind::Curried { arity, collected } => {
            let mut all = collected.clone();
            all.extend_from_slice(args);
            if all.len() >= arity {
                // Enough args gathered: run the base template with all of them.
                let base = with_host(|h| {
                    h.alloc(RObj::Proc {
                        template,
                        scope: scope.clone(),
                        is_lambda: false,
                        kind: ProcKind::Normal,
                    })
                });
                return call_proc(&base, &all);
            }
            // Still short: return a new curried proc that remembers what we have.
            return Ok(with_host(|h| {
                h.alloc(RObj::Proc {
                    template,
                    scope: scope.clone(),
                    is_lambda: false,
                    kind: ProcKind::Curried {
                        arity,
                        collected: all,
                    },
                })
            }));
        }
        ProcKind::Normal => {}
    }

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

    // The block runs in a fresh child env chained to its captured scope, so its
    // params are block-local while enclosing variables stay read/writable — and a
    // closure created inside keeps this env alive (via `Rc`) after the block ends.
    let child = child_env(scope.locals.clone());
    for (i, p) in def.params.iter().enumerate() {
        child
            .borrow_mut()
            .vars
            .insert(p.clone(), bound.get(i).cloned().unwrap_or(Value::Undef));
    }
    let block_scope = Scope {
        locals: child,
        ..scope
    };
    let prev_active = with_host(|h| h.active_scope.replace(block_scope));
    let r = run_chunk_on(def.chunk.clone());
    with_host(|h| {
        h.active_scope = prev_active;
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
    with_host(|h| h.cur_scope().block.clone())
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
pub fn raise_signal_retry() {
    with_host(|h| h.signal = Some(Signal::Retry));
}
/// Consume a pending `retry` signal, returning whether one was set.
pub fn take_retry_signal() -> bool {
    with_host(|h| {
        if matches!(h.signal, Some(Signal::Retry)) {
            h.signal = None;
            true
        } else {
            false
        }
    })
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
