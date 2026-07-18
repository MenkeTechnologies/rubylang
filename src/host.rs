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
use std::collections::HashSet;
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
    pub const NO_MATCH: u16 = 38; // [subj] -> raise NoMatchingPatternError
    pub const CALL_ARR_BLK: u16 = 39; // [name, args_array, proc] -> self call + block
    pub const CALL_METHOD_ARR_BLK: u16 = 40; // [recv, name, args_array, proc] -> method + block
    pub const GETCVAR: u16 = 41; // [name] -> class variable of self's class
    pub const SETCVAR: u16 = 42; // [name, value] -> set class variable
    pub const DEFINED_DESC: u16 = 43; // [kind, name] -> `defined?` description or nil
}

/// Sentinel bounds for beginless (`..hi`) and endless (`lo..`) ranges, carried
/// through the integer-`Range` representation. The compiler substitutes these
/// for an absent bound; index/iteration code treats them as "start"/"end".
pub const RANGE_BEGINLESS: i64 = i64::MIN;
pub const RANGE_ENDLESS: i64 = i64::MAX;

/// One deferred stage in a lazy-enumerator pipeline.
#[derive(Debug, Clone)]
pub enum LazyOp {
    Map(Value),
    Select(Value),
    Reject(Value),
    FilterMap(Value),
    FlatMap(Value),
    TakeWhile(Value),
    DropWhile(Value),
    Take(i64),
    Drop(i64),
}

/// A heap object — the Ruby reference types.
#[derive(Debug, Clone)]
pub enum RObj {
    Str(String),
    Array(Vec<Value>),
    /// A Hash: its ordered entries, the value returned for a missing key
    /// (`Hash.new(0)` stores `Int(0)`; a plain `{}` stores `Undef`/nil), and an
    /// optional default block (`Hash.new { |h,k| ... }`) called on a miss.
    Hash {
        map: IndexMap<RKey, Value>,
        default: Value,
        default_proc: Option<Value>,
    },
    Symbol(String),
    /// A `Set`: insertion-ordered, deduplicated by `RKey` (the value form of a
    /// hash key). Stores the original `Value` for iteration/`to_a`.
    Set(IndexMap<RKey, Value>),
    /// An integer that outgrew `i64` (Ruby auto-promotes; `Integer` has no
    /// fixed width). Kept normalized: never holds a value that fits in `i64`.
    BigInt(num_bigint::BigInt),
    /// An exact rational number, always stored in lowest terms.
    Rational(num_rational::BigRational),
    /// A complex number; the real and imaginary parts keep their own numeric type
    /// (Integer/Float/Rational), matching Ruby.
    Complex {
        re: Value,
        im: Value,
    },
    /// A lazy enumerator: a source (array or range value) plus a pipeline of
    /// deferred operations, pulled on demand by `first`/`take`/`force`/`to_a`.
    Lazy {
        source: Value,
        ops: Vec<LazyOp>,
    },
    Range {
        lo: i64,
        hi: i64,
        exclusive: bool,
    },
    /// A Range with Float endpoints, e.g. `1.0..2.0`. Ruby forbids iterating a
    /// Float range directly (`each`/`to_a` raise `TypeError`); it supports
    /// `step`, `min`/`max`/`begin`/`end`, and the containment predicates
    /// (`include?`/`cover?`/`===`).
    FloatRange {
        lo: f64,
        hi: f64,
        exclusive: bool,
    },
    /// A Range with String endpoints, e.g. `'a'..'e'`. Iterated with
    /// `String#succ` succession semantics.
    StrRange {
        lo: String,
        hi: String,
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
    /// A bound `Method` object (`obj.method(:name)`): the captured receiver plus
    /// the method name. `#call(*args)` routes back through dispatch on the stored
    /// receiver; `#to_proc` yields a callable that closes over both.
    Method {
        recv: Value,
        name: String,
    },
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
    /// A concrete `Enumerator`: the yielded values already materialized into a
    /// buffer, plus an external-iteration cursor. Returned by block-less
    /// `each`/`map`/`each_with_index`/… so the result answers both the
    /// Enumerable surface (delegated to `buf`) and external iteration
    /// (`next`/`peek`/`rewind`/`size`). MRI produces these lazily; we eagerly
    /// materialize finite sources, which is faithful for everything except
    /// endless generators.
    Enumerator {
        buf: Vec<Value>,
        cursor: usize,
        /// The method that produced this Enumerator (`each`, `map`, `select`,
        /// …). It selects the re-attach strategy for `with_index`/`with_object`:
        /// `map` collects block results, `select`/`reject` filter, `each`
        /// returns the receiver.
        method: String,
    },
    /// A `Time`, stored as seconds since the Unix epoch (a float, so
    /// sub-second precision and `Time - Time` Float differences are faithful).
    /// Always interpreted as UTC — the local-timezone offset is not modeled
    /// (there is no tz database), so `.utc`/`Time.utc` are exact and
    /// `.localtime` is a no-op.
    Time {
        secs: f64,
    },
    /// A `Date`, stored as whole days since the Unix epoch (1970-01-01 = 0).
    /// Uses the same proleptic-Gregorian calendar as `Time`.
    Date {
        days: i64,
    },
}

/// Julian Day Number of the Unix epoch (1970-01-01), so `jd = days + this`.
pub const UNIX_EPOCH_JDN: i64 = 2_440_588;

/// Days in month `m` (1..=12) of year `y`, accounting for leap years.
pub fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(y) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Whether `y` is a Gregorian leap year.
pub fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days from the civil calendar date `(y, m, d)` to the Unix epoch
/// (1970-01-01). Howard Hinnant's public-domain algorithm; valid for the full
/// proleptic Gregorian range. `m` is 1..=12, `d` is 1..=31.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// The civil date `(year, month, day)` for a count of days since the Unix
/// epoch. Inverse of [`days_from_civil`] (Hinnant, public domain).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
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
    Curried { arity: usize, collected: Vec<Value> },
    /// Function composition: call `first`, feed its result to `second`.
    /// `f >> g` builds `{ first: f, second: g }`; `f << g` builds `{ first: g,
    /// second: f }`.
    Composed {
        first: Box<Value>,
        second: Box<Value>,
    },
    /// A native collector block (no template) used to materialize a user
    /// `Enumerable`'s elements: calling it appends its argument to
    /// `enum_sinks[usize]` and returns nil. Never escapes to user code.
    Collect(usize),
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
    /// A class/module reference used as a Hash key (`group_by(&:class)`), keyed
    /// by class name so it compares by value and round-trips to a class ref.
    Class(String),
    /// An Array used as a Hash key (`{[1, 2] => v}`), keyed structurally by its
    /// elements (recursively) so equal arrays hash together and round-trip.
    Array(Vec<RKey>),
    /// A Range used as a Hash key: `(lo, hi, exclusive)` for an Integer range,
    /// or the String/Float endpoint variants.
    Range(i64, i64, bool),
    StrRange(String, String, bool),
    FloatRange(u64, u64, bool),
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
    /// Index of a `*rest` splat parameter, if any.
    pub splat: Option<usize>,
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
    /// `throw(tag, value)` — unwinds to the matching `catch(tag)`. The first
    /// field is the tag (matched by object identity, like Ruby), the second the
    /// thrown value (`nil` for a bare `throw tag`).
    Throw(Value, Value),
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
    /// Heap ids of objects that have been `freeze`d. Ruby's `freeze` records an
    /// object as frozen (and `frozen?` reports it); immutability itself is not
    /// enforced here, but the recorded flag is faithful to `Object#frozen?`.
    frozen: HashSet<u32>,
    /// A LIFO stack of buffers that materialize a user `Enumerable`'s elements:
    /// `new_enum_sink` pushes an empty buffer and hands back a native collector
    /// `Proc`; driving the object's `each` with that block appends every yielded
    /// value here, and `take_enum_sink` reclaims the buffer. A stack (not a single
    /// buffer) so a nested enumerable call inside `each` can't clobber the outer one.
    enum_sinks: Vec<Vec<Value>>,
    /// `Struct.new(:a, :b)` definitions: class name → (member names, keyword_init).
    /// Anonymous structs start as `Struct:N` and are renamed when first assigned
    /// to a constant (`Point = Struct.new(...)`).
    struct_defs: IndexMap<String, (Vec<String>, bool)>,
    struct_counter: u32,
    /// Class variables (`@@x`): class name → variable name → value. Shared across
    /// the class hierarchy (looked up by walking the superclass chain).
    class_vars: IndexMap<String, IndexMap<String, Value>>,
    /// `define_method`-created instance methods: class → name → block Proc.
    define_methods: IndexMap<String, IndexMap<String, Value>>,
    /// `alias_method`/`alias` mappings: class → alias name → target method name.
    method_aliases: IndexMap<String, IndexMap<String, String>>,
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
            frozen: HashSet::new(),
            enum_sinks: Vec::new(),
            struct_defs: IndexMap::new(),
            struct_counter: 0,
            class_vars: IndexMap::new(),
            define_methods: IndexMap::new(),
            method_aliases: IndexMap::new(),
        }
    }

    /// Record `v` as frozen (`Object#freeze`). Immediates and symbols are
    /// already frozen, so only heap objects need tracking.
    pub fn freeze_value(&mut self, v: &Value) {
        if let Value::Obj(id) = v {
            self.frozen.insert(*id);
        }
    }

    /// Whether `v` is frozen (`Object#frozen?`). Immediates (Integer, Float,
    /// true, false, nil) and interned Symbols are always frozen; a heap object
    /// is frozen only once `freeze` has recorded it.
    pub fn is_frozen(&self, v: &Value) -> bool {
        match v {
            Value::Obj(id) => self.as_symbol(v).is_some() || self.frozen.contains(id),
            _ => true,
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
        self.alloc(RObj::Hash {
            map,
            default: Value::Undef,
            default_proc: None,
        })
    }
    /// `Hash.new(default)` — a hash whose `[]` returns `default` for absent keys.
    pub fn new_hash_with_default(&mut self, map: IndexMap<RKey, Value>, default: Value) -> Value {
        self.alloc(RObj::Hash {
            map,
            default,
            default_proc: None,
        })
    }
    /// Set the value returned for missing keys (`Hash#default=`), in place.
    pub fn set_hash_default(&mut self, v: &Value, default: Value) {
        if let Some(RObj::Hash { default: d, .. }) = self.obj_mut(v) {
            *d = default;
        }
    }
    /// `Hash.new { |h,k| ... }` — a hash whose `[]` calls the block on a miss.
    pub fn new_hash_with_proc(&mut self, map: IndexMap<RKey, Value>, proc: Value) -> Value {
        self.alloc(RObj::Hash {
            map,
            default: Value::Undef,
            default_proc: Some(proc),
        })
    }
    /// The value `Hash#[]` yields for a missing key (nil unless `Hash.new(d)`).
    pub fn hash_default(&self, v: &Value) -> Value {
        match self.obj(v) {
            Some(RObj::Hash { default, .. }) => default.clone(),
            _ => Value::Undef,
        }
    }
    /// The default block of a hash (`Hash.new { |h,k| ... }`), if any.
    pub fn hash_default_proc(&self, v: &Value) -> Option<Value> {
        match self.obj(v) {
            Some(RObj::Hash { default_proc, .. }) => default_proc.clone(),
            _ => None,
        }
    }
    /// Build a `Set` from a sequence of values, deduplicating by key.
    pub fn new_set(&mut self, items: Vec<Value>) -> Value {
        let mut map = IndexMap::new();
        for v in items {
            let k = self.value_to_key(&v);
            map.entry(k).or_insert(v);
        }
        self.alloc(RObj::Set(map))
    }
    /// The elements of a `Set` (in insertion order), if `v` is one.
    pub fn as_set(&self, v: &Value) -> Option<Vec<Value>> {
        match self.obj(v) {
            Some(RObj::Set(map)) => Some(map.values().cloned().collect()),
            _ => None,
        }
    }
    /// Whether the set contains `item`.
    pub fn set_contains(&self, set: &Value, item: &Value) -> bool {
        let k = self.value_to_key(item);
        matches!(self.obj(set), Some(RObj::Set(map)) if map.contains_key(&k))
    }
    /// Insert `item` into the set in place; returns `true` if it was new.
    pub fn set_add(&mut self, set: &Value, item: Value) -> bool {
        let k = self.value_to_key(&item);
        if let Some(RObj::Set(map)) = self.obj_mut(set) {
            if map.contains_key(&k) {
                false
            } else {
                map.insert(k, item);
                true
            }
        } else {
            false
        }
    }
    /// Remove `item` from the set in place; returns `true` if it was present.
    pub fn set_remove(&mut self, set: &Value, item: &Value) -> bool {
        let k = self.value_to_key(item);
        if let Some(RObj::Set(map)) = self.obj_mut(set) {
            map.shift_remove(&k).is_some()
        } else {
            false
        }
    }
    /// Wrap a `BigInt` as a Ruby Integer, demoting to an immediate `Value::Int`
    /// when it fits in `i64` (so ordinary-sized results never allocate).
    pub fn new_bigint(&mut self, b: num_bigint::BigInt) -> Value {
        use num_traits::ToPrimitive;
        match b.to_i64() {
            Some(n) => Value::Int(n),
            None => self.alloc(RObj::BigInt(b)),
        }
    }
    /// The stored `BigInt` if `v` is a *promoted* Integer (not an `i64`
    /// immediate). Used to route BigInt receivers to arbitrary-precision code.
    pub fn as_promoted_bigint(&self, v: &Value) -> Option<num_bigint::BigInt> {
        match self.obj(v) {
            Some(RObj::BigInt(b)) => Some(b.clone()),
            _ => None,
        }
    }
    /// Wrap a rational as a Ruby value (always kept in lowest terms by
    /// `num-rational`; an integer-valued rational stays a `Rational`, matching
    /// Ruby — `Rational(4, 2)` is `(2/1)`, not `2`).
    pub fn new_rational(&mut self, r: num_rational::BigRational) -> Value {
        self.alloc(RObj::Rational(r))
    }
    /// Build a complex number from its parts.
    pub fn new_complex(&mut self, re: Value, im: Value) -> Value {
        self.alloc(RObj::Complex { re, im })
    }
    /// Build a lazy enumerator from a source value and an operation pipeline.
    pub fn new_lazy(&mut self, source: Value, ops: Vec<LazyOp>) -> Value {
        self.alloc(RObj::Lazy { source, ops })
    }
    /// The `(source, ops)` of a lazy enumerator, if `v` is one.
    pub fn lazy_parts(&self, v: &Value) -> Option<(Value, Vec<LazyOp>)> {
        match self.obj(v) {
            Some(RObj::Lazy { source, ops }) => Some((source.clone(), ops.clone())),
            _ => None,
        }
    }
    /// Build a concrete `Enumerator` from an already-materialized value buffer.
    /// The cursor starts at 0 (rewound). Used for the block-less form of
    /// `each`/`map`/`each_with_index`/… so the result supports both the
    /// Enumerable surface and external iteration (`next`/`peek`).
    pub fn new_enumerator(&mut self, buf: Vec<Value>, method: &str) -> Value {
        self.alloc(RObj::Enumerator {
            buf,
            cursor: 0,
            method: method.to_string(),
        })
    }
    /// The buffered values of an `Enumerator`, if `v` is one.
    pub fn enum_buf(&self, v: &Value) -> Option<Vec<Value>> {
        match self.obj(v) {
            Some(RObj::Enumerator { buf, .. }) => Some(buf.clone()),
            _ => None,
        }
    }
    /// The source method that produced this `Enumerator`, if `v` is one.
    pub fn enum_method(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::Enumerator { method, .. }) => Some(method.clone()),
            _ => None,
        }
    }
    /// External iteration: return the element at the cursor and advance it,
    /// or `None` at the end (the caller raises `StopIteration`). `peek` reads
    /// without advancing.
    pub fn enum_next(&mut self, v: &Value, advance: bool) -> Option<Value> {
        if let Some(RObj::Enumerator { buf, cursor, .. }) = self.obj_mut(v) {
            if *cursor >= buf.len() {
                return None;
            }
            let out = buf[*cursor].clone();
            if advance {
                *cursor += 1;
            }
            Some(out)
        } else {
            None
        }
    }
    /// Reset an `Enumerator`'s external-iteration cursor to the start.
    pub fn enum_rewind(&mut self, v: &Value) {
        if let Some(RObj::Enumerator { cursor, .. }) = self.obj_mut(v) {
            *cursor = 0;
        }
    }
    /// Build a `Time` from seconds since the Unix epoch (UTC).
    pub fn new_time(&mut self, secs: f64) -> Value {
        self.alloc(RObj::Time { secs })
    }
    /// The epoch seconds of a `Time`, if `v` is one.
    pub fn time_secs(&self, v: &Value) -> Option<f64> {
        match self.obj(v) {
            Some(RObj::Time { secs }) => Some(*secs),
            _ => None,
        }
    }
    /// The broken-down UTC fields of an epoch: `(year, month, day, hour, minute,
    /// second, weekday, yearday, subsecond)`. `weekday` is 0=Sunday..6=Saturday;
    /// `yearday` is 1..=366.
    pub fn time_fields(&self, secs: f64) -> (i64, i64, i64, i64, i64, i64, i64, i64, f64) {
        let whole = secs.floor() as i64;
        let subsec = secs - whole as f64;
        // Floor-divide so negative epochs land on the correct earlier day.
        let days = whole.div_euclid(86_400);
        let rem = whole.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        // 1970-01-01 was a Thursday (=4 counting from Sunday=0).
        let wday = (days.rem_euclid(7) + 4) % 7;
        let yday = days - days_from_civil(y, 1, 1) + 1;
        (y, m, d, hh, mm, ss, wday, yday, subsec)
    }
    /// Build a `Date` from a day count since the Unix epoch.
    pub fn new_date(&mut self, days: i64) -> Value {
        self.alloc(RObj::Date { days })
    }
    /// The epoch day count of a `Date`, if `v` is one.
    pub fn date_days(&self, v: &Value) -> Option<i64> {
        match self.obj(v) {
            Some(RObj::Date { days }) => Some(*days),
            _ => None,
        }
    }
    /// `Date#to_s` / `#iso8601`: `YYYY-MM-DD`.
    pub fn date_to_s(&self, days: i64) -> String {
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}")
    }
    /// `Date#inspect`: `#<Date: YYYY-MM-DD ((JDNj,0s,0n),+0s,2299161j)>` — the
    /// Julian Day Number plus the fixed Gregorian-reform day, matching MRI.
    pub fn date_inspect(&self, days: i64) -> String {
        format!(
            "#<Date: {} (({}j,0s,0n),+0s,2299161j)>",
            self.date_to_s(days),
            days + UNIX_EPOCH_JDN
        )
    }
    /// The canonical `Time#to_s` / `#inspect` text: `YYYY-MM-DD HH:MM:SS UTC`.
    /// With `subsec`, a non-zero fractional second is appended (`.5`), matching
    /// `Time#inspect`.
    pub fn time_to_s(&self, secs: f64, subsec: bool) -> String {
        let (y, m, d, hh, mm, ss, _, _, frac) = self.time_fields(secs);
        let mut out = format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}");
        if subsec && frac.abs() > f64::EPSILON {
            // Trim to the significant fractional digits, dropping the leading 0.
            let s = format!("{frac:.9}");
            let trimmed = s.trim_start_matches('0').trim_end_matches('0');
            out.push_str(trimmed);
        }
        out.push_str(" UTC");
        out
    }
    /// The `(real, imaginary)` parts of a complex number, if `v` is one.
    pub fn complex_parts(&self, v: &Value) -> Option<(Value, Value)> {
        match self.obj(v) {
            Some(RObj::Complex { re, im }) => Some((re.clone(), im.clone())),
            _ => None,
        }
    }
    /// Format `re±imi` (the body of `to_s`; `inspect` wraps it in parens).
    pub fn complex_to_s(&mut self, re: &Value, im: &Value) -> String {
        let re_s = self.to_s(re);
        let im_s = self.to_s(im);
        let sign = if im_s.starts_with('-') { "-" } else { "+" };
        format!("{re_s}{sign}{}i", im_s.trim_start_matches('-'))
    }
    /// View an integer or rational as a `BigRational`.
    pub fn as_rational(&self, v: &Value) -> Option<num_rational::BigRational> {
        match self.obj(v) {
            Some(RObj::Rational(r)) => Some(r.clone()),
            _ => self.as_bigint(v).map(num_rational::BigRational::from),
        }
    }
    /// View any Integer (`i64` immediate or promoted `BigInt`) as a `BigInt`.
    pub fn as_bigint(&self, v: &Value) -> Option<num_bigint::BigInt> {
        match v {
            Value::Int(n) => Some(num_bigint::BigInt::from(*n)),
            Value::Obj(_) => match self.obj(v) {
                Some(RObj::BigInt(b)) => Some(b.clone()),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn new_range(&mut self, lo: i64, hi: i64, exclusive: bool) -> Value {
        self.alloc(RObj::Range { lo, hi, exclusive })
    }
    pub fn new_str_range(&mut self, lo: String, hi: String, exclusive: bool) -> Value {
        self.alloc(RObj::StrRange { lo, hi, exclusive })
    }
    pub fn new_float_range(&mut self, lo: f64, hi: f64, exclusive: bool) -> Value {
        self.alloc(RObj::FloatRange { lo, hi, exclusive })
    }
    pub fn as_float_range(&self, v: &Value) -> Option<(f64, f64, bool)> {
        match self.obj(v) {
            Some(RObj::FloatRange { lo, hi, exclusive }) => Some((*lo, *hi, *exclusive)),
            _ => None,
        }
    }
    pub fn as_str_range(&self, v: &Value) -> Option<(String, String, bool)> {
        match self.obj(v) {
            Some(RObj::StrRange { lo, hi, exclusive }) => {
                Some((lo.clone(), hi.clone(), *exclusive))
            }
            _ => None,
        }
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
        matches!(
            self.obj(v),
            Some(RObj::Proc {
                is_lambda: true,
                ..
            })
        )
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
                ProcKind::Collect(_) => Some(1),
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
                    ProcKind::Composed { .. } | ProcKind::Collect(_) => return Some(v.clone()),
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
    /// Open a fresh element buffer and return a native collector `Proc` bound to
    /// it. Passing this block to a user `Enumerable`'s `each` appends every
    /// yielded element to the buffer; pair with `take_enum_sink`.
    pub fn new_enum_sink(&mut self) -> Value {
        let idx = self.enum_sinks.len();
        self.enum_sinks.push(Vec::new());
        let scope = self.cur_scope().clone();
        self.alloc(RObj::Proc {
            template: 0,
            scope,
            is_lambda: false,
            kind: ProcKind::Collect(idx),
        })
    }
    /// Reclaim the most recently opened collector buffer (LIFO with `new_enum_sink`).
    pub fn take_enum_sink(&mut self) -> Vec<Value> {
        self.enum_sinks.pop().unwrap_or_default()
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
    /// Allocate a bound `Method` object (`obj.method(:name)`).
    pub fn new_method(&mut self, recv: Value, name: &str) -> Value {
        self.alloc(RObj::Method {
            recv,
            name: name.to_string(),
        })
    }
    /// The (receiver, method-name) of a bound `Method` value (`None` otherwise).
    pub fn as_method(&self, v: &Value) -> Option<(Value, String)> {
        match self.obj(v) {
            Some(RObj::Method { recv, name }) => Some((recv.clone(), name.clone())),
            _ => None,
        }
    }
    /// `Method#arity`. For a user-defined method the arity is the count of required
    /// positional parameters, negated (`-(n+1)`) when a `*rest` splat is present.
    /// For a built-in method the exact arity is unknown here, so it reports `-1`
    /// (variadic), matching Ruby's convention for optional/variadic methods.
    pub fn method_arity(&self, recv: &Value, name: &str) -> i64 {
        let def = if let Some(cls) = self.object_class(recv) {
            self.find_method_owner(&cls, name).map(|(d, _)| d)
        } else if let Some(cls) = self.classref_name(recv) {
            self.find_class_method(&cls, name)
        } else {
            self.methods.get(name).cloned()
        };
        match def {
            Some(d) => {
                if d.splat.is_some() {
                    // Required positional params = all positional minus the splat.
                    -((d.params.len().saturating_sub(1)) as i64 + 1)
                } else {
                    d.params.len() as i64
                }
            }
            // Built-in method: real arity is not tracked; report variadic.
            None => -1,
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
            Some(RObj::Hash { map, .. }) => Some(map.clone()),
            _ => None,
        }
    }
    pub fn set_hash(&mut self, v: &Value, m: IndexMap<RKey, Value>) {
        if let Some(RObj::Hash { map, .. }) = self.obj_mut(v) {
            *map = m;
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
        matches!(
            self.obj(v),
            Some(RObj::Proc { .. }) | Some(RObj::SymProc(_))
        )
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
    /// Register a `Struct.new(...)` definition under a fresh anonymous name and
    /// return that name (used as the class of its instances until renamed).
    pub fn define_struct(&mut self, members: Vec<String>, keyword_init: bool) -> String {
        self.struct_counter += 1;
        let name = format!("Struct:{}", self.struct_counter);
        self.struct_defs
            .insert(name.clone(), (members, keyword_init));
        name
    }
    /// The `(members, keyword_init)` of a struct class, if `name` names one.
    pub fn struct_def(&self, name: &str) -> Option<(Vec<String>, bool)> {
        self.struct_defs.get(name).cloned()
    }
    /// The class name a class variable read/write resolves against, given `self`:
    /// an instance's class, or a class-reference's own name.
    pub fn cvar_owner(&self, this: &Value) -> Option<String> {
        self.object_class(this).or_else(|| self.classref_name(this))
    }
    /// Register a `define_method`-created instance method (a block Proc) on a class.
    pub fn add_define_method(&mut self, class: &str, name: &str, proc: Value) {
        self.define_methods
            .entry(class.to_string())
            .or_default()
            .insert(name.to_string(), proc);
    }
    /// A `define_method` block for `name`, walking the superclass chain.
    pub fn find_define_method(&self, class: &str, name: &str) -> Option<Value> {
        let mut cur = Some(class.to_string());
        while let Some(c) = cur {
            if let Some(p) = self.define_methods.get(&c).and_then(|m| m.get(name)) {
                return Some(p.clone());
            }
            cur = self.superclass_of(&c);
        }
        None
    }
    /// Register `alias_name` as an alias of `target` on `class`.
    pub fn add_alias(&mut self, class: &str, alias_name: &str, target: &str) {
        self.method_aliases
            .entry(class.to_string())
            .or_default()
            .insert(alias_name.to_string(), target.to_string());
    }
    /// The method an alias points to (walking the superclass chain), if any.
    pub fn find_alias(&self, class: &str, name: &str) -> Option<String> {
        let mut cur = Some(class.to_string());
        while let Some(c) = cur {
            if let Some(t) = self.method_aliases.get(&c).and_then(|m| m.get(name)) {
                return Some(t.clone());
            }
            cur = self.superclass_of(&c);
        }
        None
    }
    /// Read a class variable, walking up the superclass chain (class variables
    /// are shared across the hierarchy). `nil` if never assigned.
    pub fn get_cvar(&self, class_name: &str, var: &str) -> Value {
        let mut cur = Some(class_name.to_string());
        while let Some(c) = cur {
            if let Some(v) = self.class_vars.get(&c).and_then(|m| m.get(var)) {
                return v.clone();
            }
            cur = self.superclass_of(&c);
        }
        Value::Undef
    }
    /// Assign a class variable: reuse the ancestor that already defines it,
    /// otherwise store it on `class_name`.
    pub fn set_cvar(&mut self, class_name: &str, var: &str, val: Value) {
        let mut owner = class_name.to_string();
        let mut cur = Some(class_name.to_string());
        while let Some(c) = cur {
            if self.class_vars.get(&c).is_some_and(|m| m.contains_key(var)) {
                owner = c;
                break;
            }
            cur = self.superclass_of(&c);
        }
        self.class_vars
            .entry(owner)
            .or_default()
            .insert(var.to_string(), val);
    }
    /// Rename an anonymous struct (`Struct:N`) to the constant it was assigned to,
    /// the first time that happens — matching how Ruby names an anonymous class.
    pub fn rename_struct(&mut self, old: &str, new: &str) {
        if let Some(def) = self.struct_defs.shift_remove(old) {
            self.struct_defs.insert(new.to_string(), def);
        }
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
    /// The ancestor chain of a class (self first), including modules, matching
    /// `Module#ancestors`. Builtin types use a fixed table; user classes walk
    /// their superclass chain and included modules, then close with the
    /// `Object`/`Kernel`/`BasicObject` root.
    pub fn class_ancestry(&self, name: &str) -> Vec<String> {
        let own = |mods: &[&str]| {
            let mut v = vec![name.to_string()];
            v.extend(mods.iter().map(|s| s.to_string()));
            v.extend(["Object", "Kernel", "BasicObject"].map(String::from));
            v
        };
        match name {
            "BasicObject" => vec!["BasicObject".into()],
            "Object" => vec!["Object".into(), "Kernel".into(), "BasicObject".into()],
            // Bare modules are their own only ancestor here.
            "Kernel" | "Comparable" | "Enumerable" => vec![name.into()],
            "Numeric" => own(&["Comparable"]),
            "Integer" | "Float" | "Rational" => own(&["Numeric", "Comparable"]),
            "Complex" => own(&["Numeric"]),
            "String" | "Symbol" | "Time" | "Date" => own(&["Comparable"]),
            "Array" | "Hash" | "Range" | "Set" | "Struct" => own(&["Enumerable"]),
            _ => {
                if self.classes.contains_key(name) {
                    // A user-defined class: self, its included modules, then up
                    // the superclass chain; finally the common root.
                    let mut out = Vec::new();
                    let mut cur = Some(name.to_string());
                    while let Some(n) = cur {
                        out.push(n.clone());
                        match self.classes.get(&n) {
                            Some(def) => {
                                for m in def.includes.iter().rev() {
                                    out.push(m.clone());
                                }
                                cur = def.superclass.clone();
                            }
                            None => {
                                // Superclass is a builtin (e.g. StandardError):
                                // splice in its ancestry and stop.
                                out.pop();
                                out.extend(self.class_ancestry(&n));
                                return dedup_keep_first(out);
                            }
                        }
                    }
                    out.extend(["Object", "Kernel", "BasicObject"].map(String::from));
                    dedup_keep_first(out)
                } else if is_builtin_exception_name(name) {
                    // Exception hierarchy: <name> → Exception → Object → …
                    if name == "Exception" {
                        own(&[])
                    } else {
                        let mut v = vec![name.to_string(), "Exception".to_string()];
                        v.extend(["Object", "Kernel", "BasicObject"].map(String::from));
                        v
                    }
                } else {
                    own(&[])
                }
            }
        }
    }
    /// The direct superclass name of a class (`Module#superclass`), or `None`
    /// for `BasicObject`. Derived from the ancestry, skipping modules.
    pub fn class_superclass(&self, name: &str) -> Option<String> {
        if name == "BasicObject" {
            return None;
        }
        // User class with an explicit superclass.
        if let Some(sc) = self.superclass_of(name) {
            return Some(sc);
        }
        // Otherwise the first non-module ancestor after `name`.
        let modules = ["Kernel", "Comparable", "Enumerable"];
        self.class_ancestry(name)
            .into_iter()
            .skip(1)
            .find(|a| !modules.contains(&a.as_str()))
            .or_else(|| {
                // A user class with no explicit superclass inherits from Object.
                if self.classes.contains_key(name) {
                    Some("Object".to_string())
                } else {
                    None
                }
            })
    }
    /// The Ruby `<` class relation: `Some(true)` when `a` is a proper descendant
    /// of `b`, `Some(false)` when `a == b` or `a` is an ancestor of `b`, and
    /// `None` when the two classes are unrelated.
    pub fn class_lt(&self, a: &str, b: &str) -> Option<bool> {
        if a == b {
            return Some(false);
        }
        if self.class_ancestry(a).iter().any(|c| c == b) {
            return Some(true); // b is an ancestor of a → a < b
        }
        if self.class_ancestry(b).iter().any(|c| c == a) {
            return Some(false); // a is an ancestor of b
        }
        None
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
                | "Method"
                | "Object"
                | "BasicObject"
                | "Comparable"
                | "Enumerable"
                | "NilClass"
                | "TrueClass"
                | "FalseClass"
                | "Set"
                | "Struct"
                | "Time"
                | "Date"
                | "Math"
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
                Some(v) if matches!(self.obj(v), Some(RObj::Hash { .. })) => {
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
                Some(RObj::BigInt(b)) => b.to_string(),
                Some(RObj::Rational(r)) => format!("{}/{}", r.numer(), r.denom()),
                Some(RObj::Complex { re, im }) => self.complex_to_s(&re, &im),
                Some(RObj::Set(map)) => {
                    let items: Vec<Value> = map.values().cloned().collect();
                    let inner: Vec<String> = items.iter().map(|v| self.inspect(v)).collect();
                    format!("Set[{}]", inner.join(", "))
                }
                Some(RObj::Time { secs }) => self.time_to_s(secs, false),
                Some(RObj::Date { days }) => self.date_to_s(days),
                Some(RObj::Lazy { .. }) => "#<Enumerator::Lazy>".to_string(),
                Some(RObj::Enumerator { buf, .. }) => {
                    format!("#<Enumerator: {}>", self.inspect_array(&buf))
                }
                Some(RObj::Range { lo, hi, exclusive }) => {
                    format!("{lo}{}{hi}", if exclusive { "..." } else { ".." })
                }
                Some(RObj::FloatRange { lo, hi, exclusive }) => format!(
                    "{}{}{}",
                    fmt_float(lo),
                    if exclusive { "..." } else { ".." },
                    fmt_float(hi)
                ),
                Some(RObj::StrRange { lo, hi, exclusive }) => {
                    format!("{lo}{}{hi}", if exclusive { "..." } else { ".." })
                }
                Some(RObj::Array(items)) => self.inspect_array(&items),
                Some(RObj::Hash { map, .. }) => self.inspect_hash(&map),
                Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) => "#<Proc>".to_string(),
                Some(RObj::Method { recv, name }) => {
                    format!("#<Method: {}#{name}>", self.class_of(&recv))
                }
                Some(RObj::Regexp { source, .. }) => format!("(?-mix:{source})"),
                // MatchData#to_s is the whole matched substring (group 0).
                Some(RObj::MatchData { groups, .. }) => {
                    groups.first().and_then(|g| g.clone()).unwrap_or_default()
                }
                Some(RObj::ClassRef(n)) => n,
                Some(RObj::Object { class, ivars }) => {
                    // A struct prints `#<struct Name a=1, b=2>`; an exception
                    // object prints its message; other objects show their class.
                    if let Some((members, _)) = self.struct_def(&class) {
                        let parts: Vec<String> = members
                            .iter()
                            .map(|m| {
                                let v = ivars.get(m).cloned().unwrap_or(Value::Undef);
                                format!("{m}={}", self.inspect(&v))
                            })
                            .collect();
                        format!("#<struct {class} {}>", parts.join(", "))
                    } else {
                        match ivars.get("message") {
                            Some(m) => self.to_s(&m.clone()),
                            None => format!("#<{class}>"),
                        }
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
            Value::Str(s) => inspect_string(s),
            Value::Obj(_) => match self.obj(v).cloned() {
                Some(RObj::Str(s)) => inspect_string(&s),
                Some(RObj::Symbol(s)) => format!(":{s}"),
                Some(RObj::BigInt(b)) => b.to_string(),
                Some(RObj::Rational(r)) => format!("({}/{})", r.numer(), r.denom()),
                Some(RObj::Complex { re, im }) => format!("({})", self.complex_to_s(&re, &im)),
                Some(RObj::Set(map)) => {
                    let inner: Vec<String> = map.values().map(|v| self.inspect(v)).collect();
                    format!("Set[{}]", inner.join(", "))
                }
                Some(RObj::Array(items)) => self.inspect_array(&items),
                Some(RObj::Hash { map, .. }) => self.inspect_hash(&map),
                Some(RObj::Regexp { source, .. }) => format!("/{source}/"),
                // `Time#inspect` shows a fractional second (unlike `#to_s`).
                Some(RObj::Time { secs }) => self.time_to_s(secs, true),
                Some(RObj::Date { days }) => self.date_inspect(days),
                // A String range inspects its endpoints with quotes: `"a".."e"`.
                Some(RObj::StrRange { lo, hi, exclusive }) => {
                    format!("{lo:?}{}{hi:?}", if exclusive { "..." } else { ".." })
                }
                // `#<MatchData "ll" 1:"l">` — whole match then numbered groups.
                Some(RObj::MatchData { groups, .. }) => {
                    let whole = groups.first().and_then(|g| g.clone()).unwrap_or_default();
                    let mut out = format!("#<MatchData {whole:?}");
                    for (i, g) in groups.iter().enumerate().skip(1) {
                        match g {
                            Some(s) => out.push_str(&format!(" {i}:{}", inspect_string(s))),
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
            RKey::Str(s) => inspect_string(s),
            RKey::Sym(s) => format!(":{s}"),
            RKey::Bool(b) => b.to_string(),
            RKey::Nil => "nil".to_string(),
            RKey::FloatBits(b) => fmt_float(f64::from_bits(*b)),
            RKey::Class(n) => n.clone(),
            RKey::Array(ks) => {
                let parts: Vec<String> = ks.clone().iter().map(|k| self.key_inspect(k)).collect();
                format!("[{}]", parts.join(", "))
            }
            RKey::Range(lo, hi, excl) => format!("{lo}{}{hi}", if *excl { "..." } else { ".." }),
            RKey::StrRange(lo, hi, excl) => {
                format!("{lo:?}{}{hi:?}", if *excl { "..." } else { ".." })
            }
            RKey::FloatRange(lo, hi, excl) => format!(
                "{}{}{}",
                fmt_float(f64::from_bits(*lo)),
                if *excl { "..." } else { ".." },
                fmt_float(f64::from_bits(*hi))
            ),
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
                Some(RObj::BigInt(_)) => "Integer",
                Some(RObj::Rational(_)) => "Rational",
                Some(RObj::Complex { .. }) => "Complex",
                Some(RObj::Lazy { .. }) => "Enumerator::Lazy",
                Some(RObj::Enumerator { .. }) => "Enumerator",
                Some(RObj::Time { .. }) => "Time",
                Some(RObj::Date { .. }) => "Date",
                Some(RObj::Set(_)) => "Set",
                Some(RObj::Array(_)) => "Array",
                Some(RObj::Hash { .. }) => "Hash",
                Some(RObj::Symbol(_)) => "Symbol",
                Some(RObj::Range { .. }) => "Range",
                Some(RObj::FloatRange { .. }) => "Range",
                Some(RObj::StrRange { .. }) => "Range",
                Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) => "Proc",
                Some(RObj::Method { .. }) => "Method",
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
                Some(RObj::ClassRef(n)) => RKey::Class(n.clone()),
                Some(RObj::Array(items)) => {
                    RKey::Array(items.iter().map(|e| self.to_key(e)).collect())
                }
                Some(RObj::Range { lo, hi, exclusive }) => RKey::Range(*lo, *hi, *exclusive),
                Some(RObj::StrRange { lo, hi, exclusive }) => {
                    RKey::StrRange(lo.clone(), hi.clone(), *exclusive)
                }
                Some(RObj::FloatRange { lo, hi, exclusive }) => {
                    RKey::FloatRange(lo.to_bits(), hi.to_bits(), *exclusive)
                }
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
            RKey::Class(n) => self.class_ref(n),
            RKey::Array(ks) => {
                let items: Vec<Value> = ks.clone().iter().map(|k| self.key_to_value(k)).collect();
                self.new_array(items)
            }
            RKey::Range(lo, hi, excl) => self.new_range(*lo, *hi, *excl),
            RKey::StrRange(lo, hi, excl) => self.new_str_range(lo.clone(), hi.clone(), *excl),
            RKey::FloatRange(lo, hi, excl) => {
                self.new_float_range(f64::from_bits(*lo), f64::from_bits(*hi), *excl)
            }
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
        // Integer arithmetic that overflowed `i64`, or that involves a value
        // already promoted to `BigInt`. Division/modulo floor toward negative
        // infinity, matching Ruby.
        if let (Some(x), Some(y)) = (self.as_bigint(a), self.as_bigint(b)) {
            use num_integer::Integer as _;
            use num_traits::Zero as _;
            let arith = match op {
                Add => Some(x.clone() + &y),
                Sub => Some(x.clone() - &y),
                Mul => Some(x.clone() * &y),
                Div if !y.is_zero() => Some(x.div_floor(&y)),
                Mod if !y.is_zero() => Some(x.mod_floor(&y)),
                _ => None,
            };
            if let Some(v) = arith {
                return Ok(self.new_bigint(v));
            }
            match op {
                Div | Mod => return Err("divided by 0".to_string()),
                Lt => return Ok(Value::Bool(x < y)),
                Gt => return Ok(Value::Bool(x > y)),
                Le => return Ok(Value::Bool(x <= y)),
                Ge => return Ok(Value::Bool(x >= y)),
                _ => {}
            }
        }
        // Rational arithmetic (an integer operand is promoted to a rational). A
        // Float operand instead demotes the rational to Float, matching Ruby.
        if matches!(self.obj(a), Some(RObj::Rational(_)))
            || matches!(self.obj(b), Some(RObj::Rational(_)))
        {
            if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
                use num_traits::ToPrimitive as _;
                let to_f = |this: &Self, v: &Value| -> f64 {
                    match v {
                        Value::Float(f) => *f,
                        Value::Int(n) => *n as f64,
                        _ => this.as_rational(v).and_then(|r| r.to_f64()).unwrap_or(0.0),
                    }
                };
                let (af, bf) = (to_f(self, a), to_f(self, b));
                return Ok(match op {
                    Add => Value::Float(af + bf),
                    Sub => Value::Float(af - bf),
                    Mul => Value::Float(af * bf),
                    Div => Value::Float(af / bf),
                    Lt => Value::Bool(af < bf),
                    Gt => Value::Bool(af > bf),
                    Le => Value::Bool(af <= bf),
                    Ge => Value::Bool(af >= bf),
                    _ => Value::Float(af),
                });
            }
            if let (Some(x), Some(y)) = (self.as_rational(a), self.as_rational(b)) {
                use num_traits::Zero as _;
                let r = match op {
                    Add => Some(x.clone() + &y),
                    Sub => Some(x.clone() - &y),
                    Mul => Some(x.clone() * &y),
                    Div if !y.is_zero() => Some(x.clone() / &y),
                    _ => None,
                };
                if let Some(v) = r {
                    return Ok(self.new_rational(v));
                }
                match op {
                    Lt => return Ok(Value::Bool(x < y)),
                    Gt => return Ok(Value::Bool(x > y)),
                    Le => return Ok(Value::Bool(x <= y)),
                    Ge => return Ok(Value::Bool(x >= y)),
                    _ => {}
                }
            }
        }
        // Complex arithmetic: `(a+bi) op (c+di)`, promoting a real operand to
        // `(real, 0)`. Component operations recurse through `num_op` so the parts
        // keep their own numeric types.
        if matches!(self.obj(a), Some(RObj::Complex { .. }))
            || matches!(self.obj(b), Some(RObj::Complex { .. }))
        {
            let (ar, ai) = self
                .complex_parts(a)
                .unwrap_or_else(|| (a.clone(), Value::Int(0)));
            let (br, bi) = self
                .complex_parts(b)
                .unwrap_or_else(|| (b.clone(), Value::Int(0)));
            let result = match op {
                Add => Some((self.num_op(Add, &ar, &br)?, self.num_op(Add, &ai, &bi)?)),
                Sub => Some((self.num_op(Sub, &ar, &br)?, self.num_op(Sub, &ai, &bi)?)),
                Mul => {
                    // (ar*br - ai*bi) + (ar*bi + ai*br)i
                    let rr1 = self.num_op(Mul, &ar, &br)?;
                    let rr2 = self.num_op(Mul, &ai, &bi)?;
                    let re = self.num_op(Sub, &rr1, &rr2)?;
                    let ii1 = self.num_op(Mul, &ar, &bi)?;
                    let ii2 = self.num_op(Mul, &ai, &br)?;
                    let im = self.num_op(Add, &ii1, &ii2)?;
                    Some((re, im))
                }
                _ => None,
            };
            if let Some((re, im)) = result {
                return Ok(self.new_complex(re, im));
            }
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
            // `Array - Array`: difference, preserving order and duplicates in the
            // left operand that are absent from the right.
            (Some(RObj::Array(xs)), Sub) => {
                if let Some(RObj::Array(ys)) = self.obj(b).cloned() {
                    let kept: Vec<Value> = xs
                        .iter()
                        .filter(|v| !ys.iter().any(|w| self.eq_values(v, w)))
                        .cloned()
                        .collect();
                    return Ok(self.new_array(kept));
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
        // `Time` arithmetic and comparison. `Time - Time` is the Float number of
        // seconds between them; `Time ± Numeric` shifts by that many seconds and
        // stays a `Time`. Comparisons order two times by their epoch seconds.
        if let Some(ta) = self.time_secs(a) {
            let num_f = |v: &Value| -> Option<f64> {
                match v {
                    Value::Int(n) => Some(*n as f64),
                    Value::Float(f) => Some(*f),
                    _ => None,
                }
            };
            match op {
                Sub => {
                    if let Some(tb) = self.time_secs(b) {
                        return Ok(Value::Float(ta - tb));
                    }
                    if let Some(n) = num_f(b) {
                        return Ok(self.new_time(ta - n));
                    }
                }
                Add => {
                    if let Some(n) = num_f(b) {
                        return Ok(self.new_time(ta + n));
                    }
                }
                Lt | Gt | Le | Ge => {
                    if let Some(tb) = self.time_secs(b) {
                        return Ok(Value::Bool(cmp_ord(op, ta.total_cmp(&tb))));
                    }
                }
                _ => {}
            }
        }
        // `Date` arithmetic. `Date - Date` is the Rational number of days between
        // them (matching MRI, which yields a `Rational`); `Date ± Integer` shifts
        // by whole days and stays a `Date`. Comparisons order by day count.
        if let Some(da) = self.date_days(a) {
            match op {
                Sub => {
                    if let Some(db) = self.date_days(b) {
                        let r = num_rational::BigRational::from(num_bigint::BigInt::from(da - db));
                        return Ok(self.new_rational(r));
                    }
                    if let Some(n) = as_int(b) {
                        return Ok(self.new_date(da - n));
                    }
                }
                Add => {
                    if let Some(n) = as_int(b) {
                        return Ok(self.new_date(da + n));
                    }
                }
                Lt | Gt | Le | Ge => {
                    if let Some(db) = self.date_days(b) {
                        return Ok(Value::Bool(cmp_ord(op, da.cmp(&db))));
                    }
                }
                _ => {}
            }
        }
        // Class comparison operators (`Integer < Numeric`): true when the left
        // class is a proper subclass, false when equal or an ancestor, nil when
        // the two classes are unrelated.
        if matches!(op, Lt | Gt | Le | Ge) {
            if let (Some(x), Some(y)) = (self.classref_name(a), self.classref_name(b)) {
                let res = match op {
                    Lt => self.class_lt(&x, &y),
                    Gt => self.class_lt(&y, &x),
                    Le if x == y => Some(true),
                    Le => self.class_lt(&x, &y),
                    Ge if x == y => Some(true),
                    _ => self.class_lt(&y, &x),
                };
                return Ok(match res {
                    Some(b) => Value::Bool(b),
                    None => Value::Undef,
                });
            }
        }
        // Set `+` (union) and `-` (difference) arrive as native Add/Sub; the
        // bitwise-named set operators (`|`/`&`/`^`) route through method dispatch.
        if let Some(xs) = self.as_set(a) {
            if matches!(op, Add | Sub) {
                let ys = self
                    .as_set(b)
                    .or_else(|| self.as_array(b))
                    .unwrap_or_default();
                let in_ys = |v: &Value, this: &Self| ys.iter().any(|w| this.eq_values(v, w));
                let result: Vec<Value> = match op {
                    Add => xs.iter().chain(ys.iter()).cloned().collect(),
                    _ => xs.iter().filter(|v| !in_ys(v, self)).cloned().collect(),
                };
                return Ok(self.new_set(result));
            }
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
            // Integer equality across the i64/BigInt boundary (a promoted BigInt
            // is never equal to an i64, since it never holds an in-range value,
            // but two BigInts or a BigInt vs Int compare by value).
            _ if matches!(self.obj(a), Some(RObj::BigInt(_)))
                || matches!(self.obj(b), Some(RObj::BigInt(_))) =>
            {
                match (self.as_bigint(a), self.as_bigint(b)) {
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                }
            }
            // Rational equality (also equal to an integer of the same value).
            _ if matches!(self.obj(a), Some(RObj::Rational(_)))
                || matches!(self.obj(b), Some(RObj::Rational(_))) =>
            {
                match (self.as_rational(a), self.as_rational(b)) {
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                }
            }
            _ => {
                let (oa, ob) = (self.obj(a), self.obj(b));
                match (oa, ob) {
                    (Some(RObj::Str(x)), Some(RObj::Str(y))) => x == y,
                    (Some(RObj::Symbol(x)), Some(RObj::Symbol(y))) => x == y,
                    (Some(RObj::Array(x)), Some(RObj::Array(y))) => {
                        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.eq_values(p, q))
                    }
                    // Hash equality is order-independent: same size and every key
                    // in `x` maps to an equal value in `y`.
                    (Some(RObj::Hash { map: x, .. }), Some(RObj::Hash { map: y, .. })) => {
                        x.len() == y.len()
                            && x.iter()
                                .all(|(k, v)| y.get(k).is_some_and(|w| self.eq_values(v, w)))
                    }
                    // Complex equality compares both parts.
                    (
                        Some(RObj::Complex { re: xr, im: xi }),
                        Some(RObj::Complex { re: yr, im: yi }),
                    ) => {
                        let (xr, xi, yr, yi) = (xr.clone(), xi.clone(), yr.clone(), yi.clone());
                        self.eq_values(&xr, &yr) && self.eq_values(&xi, &yi)
                    }
                    // Set equality is order-independent membership equality.
                    (Some(RObj::Set(x)), Some(RObj::Set(y))) => {
                        x.len() == y.len() && x.keys().all(|k| y.contains_key(k))
                    }
                    // Two Ranges are equal when their endpoints and exclusivity
                    // match (integer, float, and string ranges each compare
                    // only against the same kind).
                    (
                        Some(RObj::Range {
                            lo: al,
                            hi: ah,
                            exclusive: ax,
                        }),
                        Some(RObj::Range {
                            lo: bl,
                            hi: bh,
                            exclusive: bx,
                        }),
                    ) => al == bl && ah == bh && ax == bx,
                    (
                        Some(RObj::FloatRange {
                            lo: al,
                            hi: ah,
                            exclusive: ax,
                        }),
                        Some(RObj::FloatRange {
                            lo: bl,
                            hi: bh,
                            exclusive: bx,
                        }),
                    ) => al == bl && ah == bh && ax == bx,
                    (
                        Some(RObj::StrRange {
                            lo: al,
                            hi: ah,
                            exclusive: ax,
                        }),
                        Some(RObj::StrRange {
                            lo: bl,
                            hi: bh,
                            exclusive: bx,
                        }),
                    ) => al == bl && ah == bh && ax == bx,
                    // Two class references are equal when they name the same
                    // class (`5.class == Integer`, `Integer == Integer`).
                    (Some(RObj::ClassRef(x)), Some(RObj::ClassRef(y))) => x == y,
                    // Two Times are equal when they name the same instant.
                    (Some(RObj::Time { secs: x }), Some(RObj::Time { secs: y })) => x == y,
                    // Two Dates are equal when they name the same day.
                    (Some(RObj::Date { days: x }), Some(RObj::Date { days: y })) => x == y,
                    // Two struct instances are equal when they share a class and
                    // all their members compare equal.
                    (
                        Some(RObj::Object {
                            class: cx,
                            ivars: ix,
                        }),
                        Some(RObj::Object { class: cy, .. }),
                    ) if cx == cy && self.struct_def(cx).is_some() => {
                        let members = self.struct_def(cx).unwrap().0;
                        let ix = ix.clone();
                        members.iter().all(|m| {
                            let bv = self.ivar_of(b, m);
                            self.eq_values(ix.get(m).unwrap_or(&Value::Undef), &bv)
                        })
                    }
                    _ => matches!((a, b), (Value::Obj(i), Value::Obj(j)) if i == j),
                }
            }
        }
    }
}

/// Format an `f64` the way Ruby prints a Float (always shows a decimal point).
/// Ruby's `String#inspect`: wrap in double quotes and escape with Ruby's rules —
/// the named escapes `\a\b\t\n\v\f\r\e`, `\uXXXX` (4-digit uppercase) for other
/// control chars and `0x7f`, `\"`/`\\`, and `\#` when a `#` precedes `{`/`@`/`$`
/// (so the literal reads back unambiguously). Printable and multibyte UTF-8 is
/// verbatim.
pub fn inspect_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '#' => {
                if matches!(chars.peek(), Some('{') | Some('@') | Some('$')) {
                    out.push_str("\\#");
                } else {
                    out.push('#');
                }
            }
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0b}' => out.push_str("\\v"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\u{1b}' => out.push_str("\\e"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn fmt_float(f: f64) -> String {
    if !f.is_finite() {
        return if f.is_nan() {
            "NaN".to_string()
        } else if f > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    let a = f.abs();
    // Ruby prints very large / very small magnitudes in scientific notation.
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        return sci_notation(f);
    }
    if f == f.trunc() {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// Ruby-style scientific notation: `1.8446744073709552e+19`, `1.0e-05` — a
/// mantissa that always shows a decimal point and a signed, ≥2-digit exponent.
fn sci_notation(f: f64) -> String {
    let s = format!("{f:e}"); // e.g. "1.8446744073709552e19" / "1e-5"
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let mant = if mant.contains('.') {
        mant.to_string()
    } else {
        format!("{mant}.0")
    };
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("+", exp),
    };
    format!("{mant}e{sign}{digits:0>2}")
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

/// Remove duplicate entries from an ancestry list, keeping the first occurrence
/// (a module included at several levels appears once, at its earliest position).
fn dedup_keep_first(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|x| seen.insert(x.clone()))
        .collect()
}

/// Whether `name` is a builtin exception class name (for ancestry).
fn is_builtin_exception_name(name: &str) -> bool {
    name.ends_with("Error") || name == "Exception" || name == "StopIteration"
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
    // A `throw` that escaped every `catch` is an error, mirroring Ruby's
    // `uncaught throw :tag (UncaughtThrowError)`.
    let uncaught = with_host(|h| match h.signal.take() {
        Some(Signal::Throw(tag, _)) => Some(h.inspect(&tag)),
        _ => None,
    });
    if let Some(tag) = uncaught {
        return Err(format!("uncaught throw {tag} (UncaughtThrowError)"));
    }
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
        // A `throw` must keep unwinding past this method boundary to reach its
        // `catch`; re-arm the signal so the caller's chunk halts too.
        Some(other @ Signal::Throw(..)) => {
            with_host(|h| h.signal = Some(other));
            r
        }
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
    call_proc_self(proc_val, args, None)
}

/// Like `call_proc`, but `self_override` (when given) rebinds `self` inside the
/// proc body — used for `define_method`, where the block runs as an instance
/// method with `self` = the receiver, yet still closes over its defining scope.
pub fn call_proc_self(
    proc_val: &Value,
    args: &[Value],
    self_override: Option<&Value>,
) -> Result<Value, String> {
    let (template, scope, kind) = match with_host(|h| h.obj(proc_val).cloned()) {
        Some(RObj::Proc {
            template,
            scope,
            kind,
            ..
        }) => (template, scope, kind),
        // A bound `Method` used as a block/proc (`map(&obj.method(:m))`): re-dispatch
        // the stored method on its captured receiver.
        Some(RObj::Method { recv, name }) => {
            return crate::builtins::dispatch(&recv, &name, args, None);
        }
        // A `Symbol#to_proc` proc used as a block value: send the symbol's method
        // to the first argument.
        Some(RObj::SymProc(s)) => {
            return match args.split_first() {
                Some((recv, rest)) => crate::builtins::dispatch(recv, &s, rest, None),
                None => Err("no receiver is available".to_string()),
            };
        }
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
        ProcKind::Collect(idx) => {
            // A multi-value `yield a, b` collects as an Array element, matching
            // how `to_a` groups multiple yielded values; a single value is stored
            // as-is.
            let elem = match args {
                [single] => single.clone(),
                many => with_host(|h| h.new_array(many.to_vec())),
            };
            with_host(|h| h.enum_sinks[idx].push(elem));
            return Ok(Value::Undef);
        }
        ProcKind::Normal => {}
    }

    let def = with_host(|h| h.procs[template].clone());

    // Auto-splat: a block with more than one parameter slot destructures a single
    // array argument — `pairs.each { |k, v| … }`, and also `{ |first, *rest| … }`.
    // A lone `*rest` (one slot) does not auto-splat.
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
    match def.splat {
        None => {
            for (i, p) in def.params.iter().enumerate() {
                child
                    .borrow_mut()
                    .vars
                    .insert(p.clone(), bound.get(i).cloned().unwrap_or(Value::Undef));
            }
        }
        Some(si) => {
            // Params before the splat bind positionally; the splat collects the
            // middle; params after it bind from the end.
            let after = def.params.len() - si - 1;
            for (i, p) in def.params.iter().take(si).enumerate() {
                let v = bound.get(i).cloned().unwrap_or(Value::Undef);
                child.borrow_mut().vars.insert(p.clone(), v);
            }
            let splat_end = bound.len().saturating_sub(after).max(si);
            let rest: Vec<Value> = bound
                .get(si..splat_end)
                .map(|s| s.to_vec())
                .unwrap_or_default();
            let arr = with_host(|h| h.new_array(rest));
            child.borrow_mut().vars.insert(def.params[si].clone(), arr);
            for (j, p) in def.params.iter().skip(si + 1).enumerate() {
                let v = bound.get(splat_end + j).cloned().unwrap_or(Value::Undef);
                child.borrow_mut().vars.insert(p.clone(), v);
            }
        }
    }
    let block_scope = Scope {
        locals: child,
        self_obj: self_override
            .cloned()
            .unwrap_or_else(|| scope.self_obj.clone()),
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
/// Raise a `throw(tag, value)` control signal, unwinding to the matching
/// `catch(tag)` above (see `take_throw`).
pub fn raise_signal_throw(tag: Value, value: Value) {
    with_host(|h| h.signal = Some(Signal::Throw(tag, value)));
}
/// If a pending `throw` signal carries a tag equal (by object identity, like
/// Ruby) to `tag`, consume it and return its thrown value. A non-matching throw
/// (or any other signal) is left in place so it keeps unwinding.
pub fn take_throw(tag: &Value) -> Option<Value> {
    with_host(|h| {
        if let Some(Signal::Throw(t, _)) = &h.signal {
            if t == tag {
                if let Some(Signal::Throw(_, v)) = h.signal.take() {
                    return Some(v);
                }
            }
        }
        None
    })
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
