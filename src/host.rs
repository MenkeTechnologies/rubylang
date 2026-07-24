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
use std::sync::{Arc, Mutex};

use crate::intercepts::{self, Advice};

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
/// `Arc<Mutex>` (not `Rc<RefCell>`) so the whole object heap is `Send` and can be
/// shared across `Thread`s. Under the GVL only the running thread touches any
/// env, so the mutex is always uncontended (a cheap fast-path lock).
pub type Env = Arc<Mutex<EnvData>>;

fn new_env() -> Env {
    Arc::new(Mutex::new(EnvData {
        vars: IndexMap::new(),
        parent: None,
    }))
}
fn env_with(vars: IndexMap<String, Value>) -> Env {
    Arc::new(Mutex::new(EnvData { vars, parent: None }))
}
fn child_env(parent: Env) -> Env {
    Arc::new(Mutex::new(EnvData {
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

/// `Op::Extended` ids that are NOT builtin dispatches — the DAP debug
/// line-marker. Its id namespace is `Op::Extended(id, _)`, independent of the
/// `ops` builtin ids below (which are dispatched via the extension handler
/// registered by `install`); the marker is a no-op unless the DAP debug hook is
/// installed, so normal runs ignore it.
pub mod ext {
    /// Per-statement source-line marker emitted only in `--dap` compile mode.
    pub const DBG_LINE: u16 = 1;
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
    pub const DEFINE_SINGLETON: u16 = 44; // [recv, name, synth] -> :name; def obj.m / def Klass.m
    pub const DEFINE_METHOD_DYN: u16 = 45; // [name, synth] -> :name; def under active eval target
    pub const FIRE_HOOK: u16 = 46; // [module, hook, target] -> nil; inherited/included/extended/prepended
    pub const SUPER_BLK: u16 = 47; // [args..., proc] argc=n+1 -> super with explicit args + a new block
    pub const SUPER_FWD_BLK: u16 = 48; // [proc] -> super forwarding args, with a new block
    pub const MKSTRF: u16 = 49; // [parts...] argc=n -> frozen heap String (frozen_string_literal)
    pub const MKHASH_MERGE: u16 = 50; // [hashes...] argc=n -> one merged Hash (later wins)
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
    /// `zip(a, b, …)` — pairs each element with the same-index element of every
    /// argument array (nil past the end), producing an array per element.
    Zip(Vec<Vec<Value>>),
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
    /// A native generator body for a block-less endless `Enumerable#cycle`: driven
    /// with a yielder, it repeats the captured elements forever. The yielder's
    /// limit bounds it for `first(n)`/`take(n)` exactly like a `loop {}` generator.
    CycleProc(Vec<Value>),
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
        re: fancy_regex::Regex,
    },
    /// The result of a successful `String#match` / `Regexp#match`: the group
    /// captures (index 0 is the whole match; `None` = an unmatched optional
    /// group) plus the text before and after the whole match.
    MatchData {
        groups: Vec<Option<String>>,
        /// `(name, group_index)` for each named capture `(?<name>…)`, so
        /// `MatchData#[:name]` / `#["name"]` resolves to the right group.
        names: Vec<(String, usize)>,
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
    /// A block-based generator (`Enumerator.new { |y| ... }`): the user block
    /// that drives it by sending `<<`/`yield` to a yielder. Driven on demand by
    /// `to_a`/`first`/`take`/`lazy`; each drive re-runs the block from the start
    /// (blocks are pure/re-runnable). `materialized` caches external-iteration
    /// state — `None` until the first `next`/`peek`, which runs the block to
    /// completion (so `.next` on an *infinite* generator runs forever: there are
    /// no fibers/coroutines here, so external iteration cannot pause a block).
    Generator {
        block: Value,
        materialized: Option<(Vec<Value>, usize)>,
    },
    /// The native `Enumerator::Yielder` passed to a generator block as `|y|`.
    /// `<<`/`yield` push into the collector `enum_sinks[sink]`; once the buffer
    /// reaches `limit`, `<<`/`yield` raise a break signal to unwind the block,
    /// bounding infinite `loop {}`/`while` generators for `first(n)`/`take(n)`.
    Yielder {
        sink: usize,
        limit: usize,
    },
    /// A `Fiber` (`Fiber.new { ... }`). Holds only an index into
    /// `RubyHost.fibers`; the corosensei `Coroutine` (neither Clone nor Debug)
    /// cannot live inline in this `#[derive(Clone)]` enum, so it sits in the
    /// side table exactly like `procs`/`enum_sinks`/`around_stack`.
    Fiber {
        id: u32,
    },
    /// A `Thread`. Holds an index into `RubyHost.threads` (the `JoinHandle` +
    /// shared result/done flags); the real OS thread lives in that side table.
    Thread {
        id: u32,
    },
    /// An `IO`/`File` object. Holds only an index into `RubyHost.io_handles`;
    /// the underlying `std::fs::File` is neither `Clone` nor storable inline in
    /// this `#[derive(Clone)]` enum, so it lives in the side table exactly like
    /// `fibers`. The cell's discriminant decides whether `class` is `IO` (the
    /// standard streams) or `File` (a `File.open` handle).
    IoHandle {
        id: u32,
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
    /// A `DateTime`, stored as seconds since the Unix epoch (UTC, like `Time`),
    /// but with `Date`-style arithmetic (by day) and an ISO8601 `inspect`. Uses
    /// the same proleptic-Gregorian calendar; there is no tz database, so it is
    /// UTC-only (`%z` → `+0000`, `%Z` → `UTC`, `to_s` offset always `+00:00`).
    DateTime {
        secs: f64,
    },
    /// A `SQLite3::Database` handle. Holds only an index into
    /// `RubyHost.db_handles`; the underlying `rusqlite::Connection` is neither
    /// `Clone` nor storable inline in this `#[derive(Clone)]` enum, so it lives
    /// in the side table exactly like `File`/`TCPServer` in `io_handles`.
    Db {
        id: u32,
    },
    /// A `Fiddle::Handle` — a `dlopen`ed shared library. Holds only an index
    /// into `RubyHost.fiddle_libs`; the underlying `libloading::os::unix::Library`
    /// is not `Clone` (it owns the OS `dlopen` handle), so it lives in the side
    /// table exactly like `Db`/`File`.
    FiddleHandle {
        id: u32,
    },
    /// A `Fiddle::Function` — a callable bound to a C function address plus its
    /// runtime signature. All fields are `Clone`, so it rides inline (no side
    /// table): `addr` is the resolved code pointer, `args` the argument
    /// Fiddle type codes, `ret` the return type code (MRI's small integer codes).
    FiddleFunc {
        addr: u64,
        args: Vec<i32>,
        ret: i32,
    },
    /// A `Fiddle::Pointer` — a raw memory address with an optional known byte
    /// `size`. When `owned` is `Some(id)` the pointer owns a heap buffer stored
    /// in `RubyHost.fiddle_mem` (from `Pointer.malloc`/`Pointer[str]`); `#free`
    /// releases it. A pointer returned from a C call (`TYPE_VOIDP` result) has
    /// `owned == None` and borrows memory the callee owns.
    FiddlePtr {
        addr: u64,
        size: i64,
        owned: Option<u32>,
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
    /// A native around-advice block (no template): calling it (via `yield` in an
    /// around handler) runs the intercepted method's original body once,
    /// un-advised. `usize` indexes the host's `around_stack`. Never escapes to
    /// user code as a normal proc.
    Around(usize),
}

/// A user-defined class: its optional superclass, its instance methods, the
/// modules it `include`s (searched after own methods, before the superclass),
/// the modules it `prepend`s (searched BEFORE own methods), the modules it
/// `extend`s (their instance methods become class methods), and its class
/// methods (`def self.m`).
#[derive(Clone, Default)]
pub struct ClassDef {
    pub superclass: Option<String>,
    pub methods: IndexMap<String, MethodDef>,
    pub includes: Vec<String>,
    pub prepends: Vec<String>,
    pub extends: Vec<String>,
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
    /// Proc id of a `rescue *expr` splat body (evaluates to a class or array of
    /// classes), matched at runtime in addition to `classes`. `None` when absent.
    pub splat: Option<usize>,
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
    /// The source line currently executing in this frame (updated by the DAP
    /// debug hook at each statement marker; 0 outside `--dap`).
    line: u32,
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

/// A pending around-advice weave: the intercepted call captured so a native
/// `ProcKind::Around` block can re-run it once, plus the around handlers still
/// to be applied (outermost first). Nested arounds each carry the remainder.
#[derive(Clone)]
struct AroundCall {
    handlers: Vec<String>,
    def: MethodDef,
    self_obj: Value,
    args: Vec<Value>,
    block: Option<Value>,
    method_name: Option<String>,
    def_class: Option<String>,
}

/// The Ruby runtime.
pub struct RubyHost {
    heap: Vec<RObj>,
    frames: Vec<Frame>,
    globals: IndexMap<String, Value>,
    consts: IndexMap<String, Value>,
    // `autoload :Const, "path"`: a constant registered to lazily `require "path"`
    // on first reference. Keyed by the constant's fully-qualified name
    // (`I18n::Backend`). Consumed on the triggering read so the require runs once.
    autoloads: IndexMap<String, String>,
    methods: IndexMap<String, MethodDef>,
    classes: IndexMap<String, ClassDef>,
    begins: Vec<BeginDef>,
    procs: Vec<ProcDef>,
    symbols: IndexMap<String, u32>,
    pub error: Option<String>,
    /// The exception object of the in-flight `raise`, if any (for `rescue`).
    pending_exc: Option<Value>,
    /// MRI-format backtrace frames per exception heap id, accumulated by `abort`
    /// as an exception unwinds (innermost first). Kept off the object itself so
    /// `e.instance_variables` / inspect are unchanged; the exception's heap id is
    /// stable, so a `rescue`/re-raise still finds its trace. Cleared per run
    /// (`reset_host` rebuilds the host).
    exc_backtraces: IndexMap<u32, Vec<String>>,
    /// Heap ids of String objects whose encoding is ASCII-8BIT/BINARY (from
    /// `String#b` or `force_encoding("BINARY")`). We store only UTF-8 byte content;
    /// this side table records the encoding tag so `#encoding` answers correctly
    /// without a representation change. Absent = UTF-8 (the default).
    binary_strings: HashSet<u32>,
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
    /// A LIFO stack of pending around-advice weaves (see `AroundCall`). A native
    /// `ProcKind::Around(idx)` block references `around_stack[idx]`; entries are
    /// valid only for the duration of the top-level around weave that pushed them.
    around_stack: Vec<AroundCall>,
    /// `Struct.new(:a, :b)` definitions: class name → (member names, keyword_init).
    /// Anonymous structs start as `Struct:N` and are renamed when first assigned
    /// to a constant (`Point = Struct.new(...)`).
    struct_defs: IndexMap<String, (Vec<String>, bool)>,
    /// Struct-def names that are actually `Data.define` value classes.
    data_classes: std::collections::HashSet<String>,
    struct_counter: u32,
    /// Class variables (`@@x`): class name → variable name → value. Shared across
    /// the class hierarchy (looked up by walking the superclass chain).
    class_vars: IndexMap<String, IndexMap<String, Value>>,
    /// Class-level instance variables (`@x` where `self` is a class/module, e.g.
    /// inside `def self.m` or `class << self`): class name → variable name →
    /// value. Unlike `@@` class variables these are NOT inherited.
    class_ivars: IndexMap<String, IndexMap<String, Value>>,
    /// Native attribute accessors declared at runtime (`class_eval { attr_accessor
    /// :x }`, `C.send(:attr_reader, :y)`): class → field → (has_reader, has_writer).
    /// Checked in dispatch as an `@field` get/set, so no bytecode method is
    /// synthesized. Compile-time `attr_*` still builds real methods.
    attr_accessors: IndexMap<String, IndexMap<String, (bool, bool)>>,
    /// `define_method`-created instance methods: class → name → block Proc.
    define_methods: IndexMap<String, IndexMap<String, Value>>,
    /// Per-object singleton methods (`def obj.m`, `class << obj`, and bare `def`
    /// inside `obj.instance_eval`), keyed by the object's heap id → name → method.
    singleton_methods: IndexMap<u32, IndexMap<String, MethodDef>>,
    /// `define_singleton_method`-created singletons: object heap id → name → block
    /// Proc. Proc-based (closes over its defining scope), parallel to
    /// `define_methods` but per-object rather than per-class.
    singleton_define_methods: IndexMap<u32, IndexMap<String, Value>>,
    /// `Klass.define_singleton_method`-created class methods: class name → name →
    /// block Proc. A singleton method on a *class* object is a class method, so it
    /// is inherited by subclasses (looked up through the superclass chain) — unlike
    /// per-object singletons which are keyed by a heap id (recreated per classref).
    class_define_methods: IndexMap<String, IndexMap<String, Value>>,
    /// `alias_method`/`alias` mappings: class → alias name → target method name.
    method_aliases: IndexMap<String, IndexMap<String, String>>,
    /// Live `Thread`s, indexed by `RObj::Thread.id`: the OS-thread `JoinHandle`
    /// plus the shared result/done cells the thread body publishes into. Shared
    /// (not thread-local) — a `Thread` object is visible from any thread.
    threads: Vec<ThreadCell>,
    /// `Queue`/`SizedQueue` sync structures, indexed by the object's `__qid` ivar.
    /// Each has its OWN mutex+condvar (independent of the GVL) so a blocking
    /// `pop`/`push` can wait for a producer/consumer after releasing the GVL.
    queues: Vec<Arc<QueueSync>>,
    /// `ConditionVariable` sync structures, indexed by the object's `__cvid` ivar.
    condvars: Vec<Arc<CondVarSync>>,
    /// Live `IO`/`File` objects, indexed by `RObj::IoHandle.id`. Slots 0/1/2 are
    /// pre-seeded with the standard streams (`STDOUT`/`STDERR`/`STDIN`).
    io_handles: Vec<IoCell>,
    /// Live `SQLite3::Database` handles, indexed by `RObj::Db.id`. `None` once
    /// closed. The `rusqlite::Connection` is not `Clone` (and holds a raw
    /// sqlite3 pointer), so — like `io_handles` — it lives here, never inline in
    /// the `RObj` value enum.
    db_handles: Vec<Option<DbCell>>,
    /// Live `Fiddle::Handle` libraries, indexed by `RObj::FiddleHandle.id`.
    /// `None` once closed. The `libloading` library owns the OS `dlopen` handle
    /// and is not `Clone`, so — like `db_handles` — it lives here.
    fiddle_libs: Vec<Option<FiddleLib>>,
    /// Owned heap buffers behind `Fiddle::Pointer`s created by
    /// `Pointer.malloc`/`Pointer[str]`/`Pointer.to_ptr`, indexed by
    /// `RObj::FiddlePtr.owned`. `None` once `#free`d. The buffer's heap address
    /// is stable across pushes into this outer `Vec` (only the `Box` header
    /// moves, never the bytes it points at), so a `FiddlePtr.addr` computed from
    /// `.as_ptr()` stays valid until `#free`.
    fiddle_mem: Vec<Option<Box<[u8]>>>,
}

/// One live `SQLite3::Database`, indexed by `RObj::Db.id`. Wraps the owned
/// `rusqlite::Connection` plus the `results_as_hash` flag (`db.results_as_hash =
/// true` makes `execute` return each row as a Hash keyed by column name instead
/// of an Array).
pub struct DbCell {
    conn: rusqlite::Connection,
    pub results_as_hash: bool,
}

/// One live `Fiddle::Handle`, indexed by `RObj::FiddleHandle.id`. Wraps the
/// owned `libloading` library (the OS `dlopen` handle). Kept in a side table
/// because the library is not `Clone` and must stay loaded for as long as any
/// resolved symbol address is in use. Unix-only (`os::unix`), matching the
/// crate's target set — `os::unix::Library::this()` backs `Fiddle.dlopen(nil)`,
/// which the cross-platform `libloading::Library` does not expose.
pub struct FiddleLib(libloading::os::unix::Library);

/// A column value carried between the `rusqlite` layer and the Ruby object heap.
/// Re-exports `rusqlite::types::Value` (Null/Integer/Real/Text/Blob) so the SQL
/// execution in `RubyHost::db_execute` never touches `Value`/the heap (which
/// would require a second `&mut self` borrow while the connection is borrowed).
pub type SqlVal = rusqlite::types::Value;

/// One live `IO`/`File` object, indexed by `RObj::IoHandle.id`. The three
/// standard streams are represented structurally (they route to the process
/// stdio); `File` holds the owned `std::fs::File` (`None` once closed) and the
/// path used for `#inspect`. `std::fs::File` is not `Clone`, so — like the
/// coroutines in `fibers` — it cannot live inside the `RObj` value enum and
/// sits here instead.
pub enum IoCell {
    Stdout,
    Stderr,
    Stdin,
    File {
        file: Option<std::fs::File>,
        path: String,
    },
    /// A listening `TCPServer` (`std::net::TcpListener`). `None` once closed.
    /// `local` is the bound address string (`127.0.0.1:8080`) for `#inspect`.
    /// Neither `Clone`, so — like `File` — it lives in this side table, never
    /// inline in the `RObj` value enum.
    TcpListener {
        listener: Option<std::net::TcpListener>,
        local: String,
    },
    /// A connected `TCPSocket` (`std::net::TcpStream`), from `TCPServer#accept`
    /// or `TCPSocket.new`. `None` once closed. `peer` is the remote address for
    /// `#inspect`/`#peeraddr`. `rbuf` is a read-ahead buffer so `#gets`/`#read`
    /// don't issue one syscall per byte (refilled 4 KiB at a time).
    TcpStream {
        stream: Option<std::net::TcpStream>,
        peer: String,
        rbuf: std::collections::VecDeque<u8>,
    },
}

impl IoCell {
    /// The Ruby class name for this handle: `File` for a file handle, `IO` for a
    /// standard stream (matching MRI, where `File < IO` but the streams are `IO`),
    /// `TCPServer`/`TCPSocket` for the socket handles.
    fn class_name(&self) -> &'static str {
        match self {
            IoCell::File { .. } => "File",
            IoCell::TcpListener { .. } => "TCPServer",
            IoCell::TcpStream { .. } => "TCPSocket",
            _ => "IO",
        }
    }
}

/// A `Queue`'s blocking core: items plus close state behind the queue's own
/// mutex, with a condvar to wake blocked `pop`/`push`. Independent of the GVL —
/// a waiter releases the GVL (so producers can run) and parks here instead.
struct QueueSync {
    data: Mutex<QueueData>,
    cv: std::sync::Condvar,
}

struct QueueData {
    items: std::collections::VecDeque<Value>,
    closed: bool,
    /// `Some(n)` for a `SizedQueue` (a full `push` blocks); `None` for `Queue`.
    cap: Option<usize>,
}

/// A `ConditionVariable`'s core: a monotonically increasing generation counter
/// behind a mutex, plus a condvar. `signal`/`broadcast` bump the generation; a
/// `wait` parks until the generation moves past the value it captured — so a
/// signal delivered while the waiter holds the mutex is never lost.
struct CondVarSync {
    gen: Mutex<u64>,
    cv: std::sync::Condvar,
}

/// One spawned `Thread`: the OS-thread `JoinHandle` (taken by `join`), plus the
/// shared cells its body publishes into — `result` (the block's value or a raised
/// error) and `done` (set true when the body finishes).
struct ThreadCell {
    handle: Option<std::thread::JoinHandle<()>>,
    result: Arc<Mutex<Option<Result<Value, String>>>>,
    /// The raised exception object (if the body raised), captured before the
    /// thread's context is torn down so `join`/`value` can re-raise the real
    /// object (with `#message` etc.), not just the message string.
    exc: Arc<Mutex<Option<Value>>>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

/// One suspended fiber. `coro` is `None` only while this fiber is actively
/// running (taken out across `Coroutine::resume`). `ctx` holds the fiber's
/// volatile execution context while it is suspended.
struct FiberCell {
    coro: Option<corosensei::Coroutine<Value, Value, Result<Value, String>>>,
    /// Raw pointer to the fiber body's `Yielder`, published by the coroutine
    /// closure on entry (same thread → valid for the body's lifetime). Read by
    /// `Fiber.yield` to suspend the currently running fiber.
    yielder: *const (),
    ctx: FiberContext,
    done: bool,
}

/// The mutable "execution registers" of `RubyHost` that represent *where
/// control currently is*, as opposed to the shared object heap. Swapped at
/// every fiber resume/suspend boundary so a suspended fiber's half-finished
/// scope/signal state never leaks into the resuming caller (and vice-versa).
#[derive(Default)]
struct FiberContext {
    active_scope: Option<Scope>,
    signal: Option<Signal>,
    pending_exc: Option<Value>,
    error: Option<String>,
    frames: Vec<Frame>,
    enum_sinks: Vec<Vec<Value>>,
    around_stack: Vec<AroundCall>,
}

/// One object heap per running program, protected by that program's GVL mutex.
/// A top-level run (`eval_str`/`eval_file`, or a test's `eval_to_string`) installs
/// its own VM; a Ruby `Thread` spawned inside a program installs a *clone* of its
/// spawner's handle, so threads within one program share the heap and the GVL
/// serializes them (MRI semantics) — while independent programs on other OS
/// threads are fully isolated (no shared global).
type Vm = Arc<Mutex<RubyHost>>;

thread_local! {
    /// The VM this thread is currently bound to. Unlike `GVL_GUARD`, it persists
    /// across a `gvl_leave`/`gvl_enter` safepoint cycle, and it retains an `Arc`
    /// to the `Mutex` so the host stays at a fixed address for the whole slice.
    static CURRENT_VM: RefCell<Option<Vm>> = const { RefCell::new(None) };
    /// A raw pointer into the GVL-locked host, published while THIS thread holds
    /// the GVL. Null when the thread is not running Ruby. `with_host` uses it to
    /// reach the host without re-locking (the GVL already guarantees exclusivity).
    static HOST_PTR: std::cell::Cell<*mut RubyHost> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// The GVL guard held for this thread's whole execution slice (so a safepoint
    /// deep in the call stack can drop + reacquire it to let another thread run).
    static GVL_GUARD: RefCell<Option<GvlHold>> = const { RefCell::new(None) };
}

/// A held GVL: the lock guard plus a clone of the `Arc` it was taken from. Fields
/// drop in declaration order, so `guard` (unlock) runs while `_vm` still keeps the
/// `Mutex` alive — the guard can never outlive its `Mutex`, even at `process::exit`
/// where `CURRENT_VM`'s own `Arc` may be torn down first in unspecified order.
struct GvlHold {
    // Both fields are held only for their `Drop` (RAII): `_guard` unlocks the
    // `Mutex`, then `_vm` releases the `Arc`. Declaration order is the drop order.
    _guard: std::sync::MutexGuard<'static, RubyHost>,
    _vm: Vm,
}

/// Clone of this thread's current VM handle, lazily creating a private one the
/// first time (the fallback for standalone tool/aot/lsp `with_host` calls that
/// never ran `reset_host`). The clone keeps the `Mutex` alive for the caller.
fn current_vm() -> Vm {
    CURRENT_VM.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| Arc::new(Mutex::new(RubyHost::new())))
            .clone()
    })
}

/// Bind the calling thread to `vm` (used by a spawned `Thread` to join its
/// parent's heap). Must run with the GVL released — see the invariant on
/// `gvl_enter`.
fn install_current_vm(vm: Vm) {
    CURRENT_VM.with(|c| *c.borrow_mut() = Some(vm));
}

/// Acquire the GVL: lock the current VM, publish the pointer, stash the guard.
fn gvl_enter() {
    let vm = current_vm();
    let mut guard = vm.lock().unwrap_or_else(|p| p.into_inner());
    let ptr: *mut RubyHost = &mut *guard;
    // SAFETY: the guard is stored in `GvlHold` next to a clone of `vm`, whose
    // `Arc` keeps this `Mutex` alive (at a fixed address) for the guard's whole
    // life and drops only after it. `CURRENT_VM` is additionally only swapped
    // while the GVL is released. So the `'static` extension is sound.
    let guard: std::sync::MutexGuard<'static, RubyHost> = unsafe { std::mem::transmute(guard) };
    HOST_PTR.with(|p| p.set(ptr));
    GVL_GUARD.with(|g| {
        *g.borrow_mut() = Some(GvlHold {
            _guard: guard,
            _vm: vm,
        })
    });
}

/// Release the GVL: clear the pointer and drop the guard (unlocking the host).
fn gvl_leave() {
    HOST_PTR.with(|p| p.set(std::ptr::null_mut()));
    GVL_GUARD.with(|g| *g.borrow_mut() = None);
}

/// Run `f` while holding the GVL. Re-entrant: a nested call (the thread already
/// holds it) just runs `f`. The outermost caller owns the acquire/release, so the
/// whole Ruby execution slice runs under one continuous lock — preserving the
/// atomicity MRI's GVL provides.
pub fn with_gvl<R>(f: impl FnOnce() -> R) -> R {
    if HOST_PTR.with(|p| !p.get().is_null()) {
        return f();
    }
    gvl_enter();
    let r = f();
    gvl_leave();
    r
}

/// Temporarily release the GVL around a blocking operation (`Thread#join`,
/// `Queue#pop` on empty, `sleep`), letting another thread run, then reacquire.
/// A no-op when the GVL is not held (single-threaded tool/test contexts).
pub fn gvl_blocking<R>(blocking: impl FnOnce() -> R) -> R {
    if HOST_PTR.with(|p| p.get().is_null()) {
        return blocking();
    }
    gvl_leave();
    let r = blocking();
    gvl_enter();
    r
}

/// Run `f` with mutable access to the shared host. When the GVL is held (normal
/// execution), it reaches the host through the published pointer — no re-lock,
/// since the GVL already guarantees this thread exclusive access. Outside a GVL
/// slice (standalone tool/test calls) it locks the host just for this call.
pub fn with_host<R>(f: impl FnOnce(&mut RubyHost) -> R) -> R {
    let ptr = HOST_PTR.with(|p| p.get());
    if !ptr.is_null() {
        // SAFETY: this thread holds the GVL for the whole slice, so `ptr` points
        // at the locked host and no other thread can touch it concurrently.
        return f(unsafe { &mut *ptr });
    }
    let vm = current_vm();
    let mut guard = vm.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

/// Begin a fresh program: install a brand-new VM on this thread, so an
/// independent run never shares state with (or corrupts) a program running
/// concurrently on another OS thread. Must run with the GVL released — the
/// `'static` guard extension in `gvl_enter` depends on the current VM not being
/// swapped out while a guard into it is live.
pub fn reset_host() {
    debug_assert!(
        GVL_GUARD.with(|g| g.borrow().is_none()),
        "reset_host must run with the GVL released"
    );
    CURRENT_VM.with(|c| *c.borrow_mut() = Some(Arc::new(Mutex::new(RubyHost::new()))));
    crate::intercepts::clear();
    FILE_DIR_STACK.with(|s| s.borrow_mut().clear());
    FILE_PATH_STACK.with(|s| s.borrow_mut().clear());
    DEF_TARGET.with(|t| t.borrow_mut().clear());
    // Fibers moved off the host into a thread-local, so clear them explicitly.
    FIBERS.with(|f| f.borrow_mut().clear());
    CUR_FIBER.with(|c| c.set(None));
    CURRENT_THREAD.with(|c| *c.borrow_mut() = None);
}

thread_local! {
    /// The directory of the file currently being run, as a stack: pushed before a
    /// `require`/`require_relative`/`load`d file runs and popped after, plus the
    /// top-level script's dir at the bottom. `require_relative` resolves against
    /// the top entry (the requiring file's dir).
    static FILE_DIR_STACK: RefCell<Vec<std::path::PathBuf>> = const { RefCell::new(Vec::new()) };
    /// The path of the file currently being run, pushed/popped in lockstep with
    /// `FILE_DIR_STACK`; the top entry is what `__FILE__` reports.
    static FILE_PATH_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Push the directory of the file about to run (see `FILE_DIR_STACK`).
pub fn push_file_dir(dir: std::path::PathBuf) {
    FILE_DIR_STACK.with(|s| s.borrow_mut().push(dir));
}

/// Push the path of the file about to run (see `FILE_PATH_STACK`), the value
/// `__FILE__` reports while it runs. Pushed in lockstep with `push_file_dir`.
pub fn push_file_path(path: String) {
    FILE_PATH_STACK.with(|s| s.borrow_mut().push(path));
}

/// Pop after a required/loaded file finishes running.
pub fn pop_file_dir() {
    FILE_DIR_STACK.with(|s| {
        s.borrow_mut().pop();
    });
    FILE_PATH_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

/// The directory of the file currently running (top of the stack), for
/// `require_relative` resolution.
pub fn current_file_dir() -> Option<std::path::PathBuf> {
    FILE_DIR_STACK.with(|s| s.borrow().last().cloned())
}

/// The path of the file currently running (top of the stack), for `__FILE__`.
pub fn current_file_path() -> Option<String> {
    FILE_PATH_STACK.with(|s| s.borrow().last().cloned())
}

/// Where a bare `def` should register while a `class_eval`/`instance_eval`
/// target is active. `Instance` = an instance method on the class (class_eval),
/// `ClassMethod` = a class/singleton method (`Klass.instance_eval`), `Singleton`
/// = a per-object singleton (`obj.instance_eval`), `None` = an ordinary method
/// body inside an eval (defs there hoist as usual, not onto the eval target).
#[derive(Clone)]
pub enum DefTarget {
    Instance(String),
    ClassMethod(String),
    Singleton(u32),
    None,
}

thread_local! {
    /// A dynamically-scoped stack of the active `def` target(s). Empty during
    /// normal execution; a `class_eval`/`instance_eval` pushes its target for the
    /// duration of the body, and every method call nested underneath pushes a
    /// `None` so defs inside called methods hoist normally.
    static DEF_TARGET: RefCell<Vec<DefTarget>> = const { RefCell::new(Vec::new()) };
}

/// Register a runtime `def` (identified by its real name and the synthetic name
/// its body was stashed under) onto the active eval target, if any.
pub fn apply_def_target(real: &str, synth: &str) {
    let target = DEF_TARGET.with(|t| t.borrow().last().cloned());
    let Some(target) = target else {
        return;
    };
    let Some(def) = with_host(|h| h.method_def(synth)) else {
        return;
    };
    with_host(|h| match target {
        DefTarget::Instance(c) => h.add_instance_method(&c, real, def),
        DefTarget::ClassMethod(c) => h.add_class_method(&c, real, def),
        DefTarget::Singleton(id) => h.add_singleton_method(id, real, def),
        DefTarget::None => {}
    });
}

/// Run a block with `self` rebound to `self_val` and `target` as the active
/// `def` target for its duration (`class_eval`/`instance_eval`/`instance_exec`
/// block forms). `args` are the block's arguments.
pub fn eval_block_scoped(
    block: &Value,
    self_val: &Value,
    target: DefTarget,
    args: &[Value],
) -> Result<Value, String> {
    DEF_TARGET.with(|t| t.borrow_mut().push(target));
    let r = call_proc_self(block, args, Some(self_val));
    DEF_TARGET.with(|t| t.borrow_mut().pop());
    r
}

/// Compile and run `src` with `self` rebound to `self_val` and `target` as the
/// active `def` target (string `class_eval`/`instance_eval`). Methods, classes,
/// and constants it defines persist on the host.
pub fn eval_string_scoped(src: &str, self_val: &Value, target: DefTarget) -> Result<Value, String> {
    DEF_TARGET.with(|t| t.borrow_mut().push(target));
    with_host(|h| {
        h.frames.push(Frame {
            scope: Scope {
                locals: new_env(),
                block: None,
                self_obj: self_val.clone(),
                method_name: None,
                def_class: None,
            },
            args: Vec::new(),
            line: 0,
        });
    });
    let saved_active = with_host(|h| h.active_scope.take());
    let r = eval_in_place(src);
    with_host(|h| {
        h.frames.pop();
        h.active_scope = saved_active;
    });
    DEF_TARGET.with(|t| t.borrow_mut().pop());
    r
}

/// Evaluate ERB-compiled `src` in a fresh, isolated top-level scope with
/// `locals` pre-bound. Used by `ERB#result_with_hash`, whose hash keys become
/// template locals in a binding that does not see (or pollute) the caller's
/// variables. `self` is a blank `Object`, so the template's instance-variable
/// reads start empty — matching MRI's `result_with_hash` (a new binding).
pub fn eval_erb_with_locals(src: &str, locals: Vec<(String, Value)>) -> Result<Value, String> {
    let self_obj = with_host(|h| h.new_object("Object"));
    let env = new_env();
    {
        let mut e = env.lock().unwrap();
        for (k, v) in locals {
            e.vars.insert(k, v);
        }
    }
    with_host(|h| {
        h.frames.push(Frame {
            scope: Scope {
                locals: env,
                block: None,
                self_obj,
                method_name: None,
                def_class: None,
            },
            args: Vec::new(),
            line: 0,
        });
    });
    let saved_active = with_host(|h| h.active_scope.take());
    let r = eval_in_place(src);
    with_host(|h| {
        h.frames.pop();
        h.active_scope = saved_active;
    });
    r
}

/// `eval("code")` at top level / current self: compile `src` into the running
/// host (its proc/begin templates appended at the right offset) and run its main
/// chunk in the current frame. Definitions persist; returns the last value.
pub fn eval_in_place(src: &str) -> Result<Value, String> {
    let stmts = crate::parser::parse(src)?;
    let (proc_base, begin_base) = with_host(|h| (h.procs.len(), h.begins.len()));
    let prog = crate::compiler::compile_at(&stmts, proc_base, begin_base)?;
    let main = prog.main;
    with_host(|h| {
        for (name, def) in prog.methods {
            h.methods.insert(name, def);
        }
        for (name, def) in prog.classes {
            merge_class(&mut h.classes, name, def);
        }
        h.begins.extend(prog.begins);
        h.procs.extend(prog.procs);
    });
    run_chunk_on(main)
}

/// Invoke a per-object singleton method (`def obj.m`): push a frame bound to the
/// object and run the body, resolving `super` against the object's own class.
pub fn call_singleton(
    recv: Value,
    def: &MethodDef,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    let def_class = with_host(|h| h.class_of(&recv));
    run_method(def, recv, args, block, Some(name.into()), Some(def_class))
}

/// Merge a class/module definition into the store, implementing Ruby's
/// "reopening adds to the class" semantics. A second `class A … end` (or
/// `module M … end`) does NOT replace the first: its methods and class methods
/// are added (a redefined name replaces the earlier body, as in MRI), and its
/// `include`/`prepend`/`extend` mixins accumulate. Synthetic `__class_body__N`
/// entries are already uniquely named per opening, so they coexist. If no class
/// of this name exists yet, the definition is installed as-is.
fn merge_class(classes: &mut IndexMap<String, ClassDef>, name: String, def: ClassDef) {
    let Some(existing) = classes.get_mut(&name) else {
        classes.insert(name, def);
        return;
    };
    // A reopening usually omits the superclass; adopt a new one only if given.
    if def.superclass.is_some() {
        existing.superclass = def.superclass;
    }
    for (k, v) in def.methods {
        existing.methods.insert(k, v);
    }
    for (k, v) in def.class_methods {
        existing.class_methods.insert(k, v);
    }
    for m in def.includes {
        if !existing.includes.contains(&m) {
            existing.includes.push(m);
        }
    }
    for m in def.prepends {
        if !existing.prepends.contains(&m) {
            existing.prepends.push(m);
        }
    }
    for m in def.extends {
        if !existing.extends.contains(&m) {
            existing.extends.push(m);
        }
    }
}

impl Default for RubyHost {
    fn default() -> Self {
        Self::new()
    }
}

impl RubyHost {
    pub fn new() -> Self {
        // MRI's top-level `self` is `main`, an ordinary Object — so
        // `self.class.name == "Object"`. It occupies heap slot 0.
        let main = RObj::Object {
            class: "Object".to_string(),
            ivars: IndexMap::new(),
        };
        let mut h = RubyHost {
            heap: vec![main],
            frames: vec![Frame {
                scope: Scope {
                    locals: new_env(),
                    block: None,
                    self_obj: Value::Obj(0),
                    method_name: None,
                    def_class: None,
                },
                args: Vec::new(),
                line: 0,
            }],
            globals: IndexMap::new(),
            consts: IndexMap::new(),
            autoloads: IndexMap::new(),
            methods: IndexMap::new(),
            classes: IndexMap::new(),
            begins: Vec::new(),
            procs: Vec::new(),
            symbols: IndexMap::new(),
            error: None,
            pending_exc: None,
            exc_backtraces: IndexMap::new(),
            binary_strings: HashSet::new(),
            signal: None,
            active_scope: None,
            frozen: HashSet::new(),
            enum_sinks: Vec::new(),
            around_stack: Vec::new(),
            threads: Vec::new(),
            queues: Vec::new(),
            condvars: Vec::new(),
            io_handles: vec![IoCell::Stdout, IoCell::Stderr, IoCell::Stdin],
            db_handles: Vec::new(),
            fiddle_libs: Vec::new(),
            fiddle_mem: Vec::new(),
            struct_defs: IndexMap::new(),
            data_classes: std::collections::HashSet::new(),
            struct_counter: 0,
            class_vars: IndexMap::new(),
            class_ivars: IndexMap::new(),
            attr_accessors: IndexMap::new(),
            define_methods: IndexMap::new(),
            singleton_methods: IndexMap::new(),
            singleton_define_methods: IndexMap::new(),
            class_define_methods: IndexMap::new(),
            method_aliases: IndexMap::new(),
        };
        // Seed the standard streams as `STDOUT`/`STDERR`/`STDIN` constants and
        // the `$stdout`/`$stderr`/`$stdin` globals. Slots 0/1/2 in `io_handles`
        // hold the corresponding `IoCell`s (see the field initializer above).
        let stdout = h.alloc(RObj::IoHandle { id: 0 });
        let stderr = h.alloc(RObj::IoHandle { id: 1 });
        let stdin = h.alloc(RObj::IoHandle { id: 2 });
        h.set_const("STDOUT", stdout.clone());
        h.set_const("STDERR", stderr.clone());
        h.set_const("STDIN", stdin.clone());
        h.set_global("stdout", stdout);
        h.set_global("stderr", stderr);
        h.set_global("stdin", stdin);
        // Ruby identity constants. `RUBY_ENGINE` names rubylang honestly (engine
        // split, like JRuby/TruffleRuby); `RUBY_VERSION` is the MRI language level
        // targeted so gems' `required_ruby_version` checks pass.
        let ver = h.new_string(crate::RUBY_COMPAT_VERSION.to_string());
        h.set_const("RUBY_VERSION", ver);
        let engine = h.new_string(crate::RUBY_ENGINE.to_string());
        h.set_const("RUBY_ENGINE", engine);
        let engine_ver = h.new_string(crate::RUBY_ENGINE_VERSION.to_string());
        h.set_const("RUBY_ENGINE_VERSION", engine_ver);
        let platform = h.new_string(crate::ruby_platform());
        h.set_const("RUBY_PLATFORM", platform);
        let desc = h.new_string(crate::version_banner());
        h.set_const("RUBY_DESCRIPTION", desc);
        h.set_const("RUBY_PATCHLEVEL", Value::Int(-1));
        h
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
            // Ranges are immutable and always frozen (MRI 3.0+), as are Symbols;
            // any other heap object is frozen only once explicitly `freeze`d.
            Value::Obj(id) => {
                self.as_symbol(v).is_some()
                    || matches!(
                        self.obj(v),
                        Some(RObj::Range { .. } | RObj::FloatRange { .. } | RObj::StrRange { .. })
                    )
                    || self.frozen.contains(id)
            }
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
            merge_class(&mut self.classes, name, def);
        }
        // Append, never replace: a `require`/`load` (or each REPL line) merges a
        // second program onto the live host. Its ids were already rebased above
        // the current lengths by `compiler::rebase_program`, so appending keeps
        // every already-loaded proc/begin id valid.
        self.begins.extend(begins);
        self.procs.extend(procs);
    }

    /// The base a freshly compiled program must be rebased by before it is merged
    /// so its proc/begin ids don't collide with what is already loaded:
    /// (`procs.len()`, `begins.len()`). See `compiler::rebase_program`.
    pub fn program_offsets(&self) -> (usize, usize) {
        (self.procs.len(), self.begins.len())
    }

    /// Seed `$LOAD_PATH`/`$:` (an Array holding `dir`) and `$LOADED_FEATURES`/`$"`
    /// (an empty Array). Each alias pair points at the *same* heap Array object so
    /// a push through either name is visible through the other, matching Ruby's
    /// `$LOAD_PATH.equal?($:)`.
    pub fn init_load_path(&mut self, dir: &str) {
        // The script dir first, then every installed gem's `lib/` dir — modern
        // Ruby auto-activates RubyGems, putting gem libs on $LOAD_PATH, so
        // `require "some_gem"` resolves. rubylang mirrors that (drop-in intent).
        let mut entries = vec![self.new_string(dir.to_string())];
        for gd in gem_lib_dirs() {
            entries.push(self.new_string(gd));
        }
        let load_path = self.new_array(entries);
        self.set_global("LOAD_PATH", load_path.clone());
        self.set_global(":", load_path);
        let features = self.new_array(Vec::new());
        self.set_global("LOADED_FEATURES", features.clone());
        self.set_global("\"", features);
    }

    /// Seed the program arguments the way `ruby(1)` does: `ARGV`/`$*` is an Array
    /// of the post-script command-line arguments, and `$0`/`$PROGRAM_NAME` is the
    /// script name (the file path, `-e` for a one-liner, or `-` for stdin).
    pub fn set_program_args(&mut self, argv: &[String], script_name: &str) {
        let items: Vec<Value> = argv.iter().map(|a| self.new_string(a.clone())).collect();
        let arr = self.new_array(items);
        self.set_const("ARGV", arr.clone());
        self.set_global("*", arr);
        let name = self.new_string(script_name.to_string());
        self.set_global("0", name.clone());
        self.set_global("PROGRAM_NAME", name);
    }

    /// Prepend `-I` directories to the front of `$LOAD_PATH` (MRI resolves `-I`
    /// dirs before the script dir and gem libs). Must run after `init_load_path`.
    pub fn prepend_load_path(&mut self, dirs: &[String]) {
        if dirs.is_empty() {
            return;
        }
        let lp = self.get_global("LOAD_PATH");
        let news: Vec<Value> = dirs.iter().map(|d| self.new_string(d.clone())).collect();
        if let Some(RObj::Array(items)) = self.obj_mut(&lp) {
            for (i, v) in news.into_iter().enumerate() {
                items.insert(i, v);
            }
        }
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
    /// Build a block-based generator (`Enumerator.new { |y| ... }`).
    pub fn new_generator(&mut self, block: Value) -> Value {
        self.alloc(RObj::Generator {
            block,
            materialized: None,
        })
    }
    /// A block-less endless `cycle` Enumerator: a `Generator` whose body is a
    /// native `CycleProc` repeating `buf` forever (bounded by the consumer via
    /// `first(n)`/`take(n)`).
    pub fn new_cycle_enumerator(&mut self, buf: Vec<Value>) -> Value {
        let block = self.alloc(RObj::CycleProc(buf));
        self.new_generator(block)
    }
    /// The driving block of a `Generator`, if `v` is one.
    pub fn generator_block(&self, v: &Value) -> Option<Value> {
        match self.obj(v) {
            Some(RObj::Generator { block, .. }) => Some(block.clone()),
            _ => None,
        }
    }
    /// Whether `v` is a generator that has not yet been materialized.
    pub fn generator_unmaterialized(&self, v: &Value) -> bool {
        matches!(
            self.obj(v),
            Some(RObj::Generator {
                materialized: None,
                ..
            })
        )
    }
    /// Open a fresh sink and return a `Yielder` bound to it that stops the
    /// generator after `limit` values (`usize::MAX` = run to completion). Pair
    /// with `take_enum_sink` once the drive returns.
    pub fn new_yielder(&mut self, limit: usize) -> Value {
        let sink = self.enum_sinks.len();
        self.enum_sinks.push(Vec::new());
        self.alloc(RObj::Yielder { sink, limit })
    }
    /// Push a value produced by a `Yielder`'s `<<`/`yield`. Returns `true` when
    /// the sink has reached its limit (the caller raises a break signal).
    pub fn yielder_push(&mut self, v: &Value, val: Value) -> bool {
        if let Some(RObj::Yielder { sink, limit }) = self.obj(v).cloned() {
            let len = self.enum_sinks.get(sink).map(|s| s.len()).unwrap_or(0);
            if len >= limit {
                return true;
            }
            if let Some(s) = self.enum_sinks.get_mut(sink) {
                s.push(val);
            }
            return len + 1 >= limit;
        }
        false
    }
    /// Cache a `Generator`'s fully-materialized buffer for external iteration.
    pub fn set_generator_materialized(&mut self, v: &Value, buf: Vec<Value>) {
        if let Some(RObj::Generator { materialized, .. }) = self.obj_mut(v) {
            *materialized = Some((buf, 0));
        }
    }
    /// The element count of a `CycleProc` generator body, or `None` if `block` is
    /// not one. Lets the `next`/`peek` path materialize a single cycle instead of
    /// hanging on the endless drive.
    pub fn cycle_proc_len(&self, block: &Value) -> Option<usize> {
        match self.obj(block) {
            Some(RObj::CycleProc(buf)) => Some(buf.len()),
            _ => None,
        }
    }
    /// External iteration over a materialized generator; `None` past the end. A
    /// `cycle` generator (its body is a `CycleProc`) wraps its cursor forever
    /// rather than ending, so `e.next` round-robins the buffer.
    pub fn generator_next(&mut self, v: &Value, advance: bool) -> Option<Value> {
        let is_cycle = match self.generator_block(v) {
            Some(b) => matches!(self.obj(&b), Some(RObj::CycleProc(_))),
            None => false,
        };
        if let Some(RObj::Generator {
            materialized: Some((buf, cursor)),
            ..
        }) = self.obj_mut(v)
        {
            if buf.is_empty() {
                return None;
            }
            if *cursor >= buf.len() {
                if is_cycle {
                    *cursor = 0;
                } else {
                    return None;
                }
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
    /// Reset a materialized generator's external-iteration cursor.
    pub fn generator_rewind(&mut self, v: &Value) {
        if let Some(RObj::Generator {
            materialized: Some((_, cursor)),
            ..
        }) = self.obj_mut(v)
        {
            *cursor = 0;
        }
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
    /// Build a `DateTime` from seconds since the Unix epoch (UTC).
    pub fn new_datetime(&mut self, secs: f64) -> Value {
        self.alloc(RObj::DateTime { secs })
    }
    /// The epoch seconds of a `DateTime`, if `v` is one.
    pub fn datetime_secs(&self, v: &Value) -> Option<f64> {
        match self.obj(v) {
            Some(RObj::DateTime { secs }) => Some(*secs),
            _ => None,
        }
    }
    /// `DateTime#to_s` / `#iso8601`: `YYYY-MM-DDTHH:MM:SS+00:00` (UTC-only).
    pub fn datetime_to_s(&self, secs: f64) -> String {
        let (y, mo, d, hh, mi, ss, _, _, _) = self.time_fields(secs);
        format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mi:02}:{ss:02}+00:00")
    }
    /// `DateTime#inspect`: the ISO8601 form plus the Julian Day Number, the
    /// seconds-since-midnight, and the nanosecond fraction, matching MRI.
    pub fn datetime_inspect(&self, secs: f64) -> String {
        let (_, _, _, hh, mi, ss, _, _, frac) = self.time_fields(secs);
        let day = (secs / 86_400.0).floor() as i64;
        let sod = hh * 3600 + mi * 60 + ss;
        let nsec = (frac * 1e9).round() as i64;
        format!(
            "#<DateTime: {} (({}j,{}s,{}n),+0s,2299161j)>",
            self.datetime_to_s(secs),
            day + UNIX_EPOCH_JDN,
            sod,
            nsec
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
    /// Compile a regex literal (Ruby `flags` → inline flags: `i`
    /// case-insensitive, `m` dot-matches-newline, `x` extended). Returns an error
    /// string if the pattern is not valid for the fancy-regex engine.
    ///
    /// fancy-regex is a backtracking engine, so Ruby/Onigmo features the `regex`
    /// crate rejects — backreferences (`\1`, `\k<name>`) and look-around
    /// (`(?=…)`, `(?<=…)`) — compile and match here. Ruby anchors (`\A`/`\z`/
    /// `\Z`/`\G`), `\h`/`\H`, named groups, and POSIX classes are all supported
    /// by its parser, so patterns pass through unrewritten.
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
        match fancy_regex::Regex::new(&full) {
            Ok(re) => Ok(self.alloc(RObj::Regexp {
                source: source.to_string(),
                re,
            })),
            Err(e) => Err(format!("invalid regex /{source}/: {e}")),
        }
    }
    /// The `(groups, names, pre, post)` of a `MatchData` value, if `v` is one.
    #[allow(clippy::type_complexity)]
    pub fn as_matchdata(
        &self,
        v: &Value,
    ) -> Option<(Vec<Option<String>>, Vec<(String, usize)>, String, String)> {
        match self.obj(v) {
            Some(RObj::MatchData {
                groups,
                names,
                pre,
                post,
            }) => Some((groups.clone(), names.clone(), pre.clone(), post.clone())),
            _ => None,
        }
    }
    /// The compiled matcher + source of a regex value, if `v` is one.
    pub fn as_regex(&self, v: &Value) -> Option<(fancy_regex::Regex, String)> {
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
        names: Vec<(String, usize)>,
        pre: String,
        post: String,
    ) -> Value {
        self.alloc(RObj::MatchData {
            groups,
            names,
            pre,
            post,
        })
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
                ProcKind::Collect(_) | ProcKind::Around(_) => Some(1),
                ProcKind::Normal => {
                    let def = &self.procs[*template];
                    if def.splat.is_some() {
                        // A block/lambda with a splat has arity -(required + 1),
                        // the required count being all positional params bar the
                        // splat itself.
                        Some(-(def.params.len().saturating_sub(1) as i64 + 1))
                    } else {
                        Some(def.params.len() as i64)
                    }
                }
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
                    ProcKind::Composed { .. } | ProcKind::Collect(_) | ProcKind::Around(_) => {
                        return Some(v.clone())
                    }
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
    /// Push a pending around weave and return its index (for a `ProcKind::Around` block).
    #[allow(clippy::too_many_arguments)]
    fn push_around(
        &mut self,
        handlers: Vec<String>,
        def: MethodDef,
        self_obj: Value,
        args: Vec<Value>,
        block: Option<Value>,
        method_name: Option<String>,
        def_class: Option<String>,
    ) -> usize {
        let idx = self.around_stack.len();
        self.around_stack.push(AroundCall {
            handlers,
            def,
            self_obj,
            args,
            block,
            method_name,
            def_class,
        });
        idx
    }
    /// Clone the around weave at `idx` (a native `ProcKind::Around` block target).
    fn around_call(&self, idx: usize) -> AroundCall {
        self.around_stack[idx].clone()
    }
    /// Current around-stack depth (checkpoint for `truncate_around`).
    fn around_len(&self) -> usize {
        self.around_stack.len()
    }
    /// Drop around weaves pushed since a checkpoint (bounds the stack per call).
    fn truncate_around(&mut self, n: usize) {
        self.around_stack.truncate(n);
    }
    /// Allocate a native around-advice block bound to `around_stack[idx]`.
    fn new_around_block(&mut self, idx: usize) -> Value {
        let scope = self.cur_scope().clone();
        self.alloc(RObj::Proc {
            template: 0,
            scope,
            is_lambda: false,
            kind: ProcKind::Around(idx),
        })
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
    /// `Method#parameters` descriptors: `(kind, name)` pairs. Optional-vs-required
    /// and keyreq-vs-key aren't tracked (no default info on `MethodDef`), so
    /// positional params report `req` and keyword params `key`; the splat/kwsplat/
    /// block kinds (which delegation forwarding actually keys on) are exact.
    pub fn method_parameters(&self, recv: &Value, name: &str) -> Vec<(&'static str, String)> {
        let def = if let Some(cls) = self.object_class(recv) {
            self.find_method_owner(&cls, name).map(|(d, _)| d)
        } else if let Some(cls) = self.classref_name(recv) {
            self.find_class_method(&cls, name)
        } else {
            self.methods.get(name).cloned()
        };
        let mut out = Vec::new();
        if let Some(d) = def {
            for (i, p) in d.params.iter().enumerate() {
                let pname = p.trim_start_matches('*').to_string();
                out.push((if Some(i) == d.splat { "rest" } else { "req" }, pname));
            }
            for k in &d.kwparams {
                out.push(("key", k.clone()));
            }
            if let Some(ks) = &d.kwsplat {
                out.push(("keyrest", ks.trim_start_matches('*').to_string()));
            }
            if let Some(bp) = &d.blockparam {
                out.push(("block", bp.trim_start_matches('&').to_string()));
            }
        }
        out
    }
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
            Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) | Some(RObj::CycleProc(_))
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
    /// Whether the class/module `cls` responds to `name` as a class method: its
    /// own class methods, a `define_singleton_method`, an instance method on its
    /// singleton class (`Klass.singleton_class.class_eval { def m }`), the common
    /// Module/Class reflection surface, and inherited Class/Module instance
    /// methods. Used by both `responds_to` (bare self-calls) and `respond_to?` on
    /// a class receiver — sinatra's `set` DSL branches on `respond_to?("opt=")`.
    pub fn class_responds_to(&self, cls: &str, name: &str) -> bool {
        name == "new"
            || matches!(
                name,
                "name"
                    | "to_s"
                    | "inspect"
                    | "singleton_class"
                    | "instance_methods"
                    | "public_instance_methods"
                    | "private_instance_methods"
                    | "class_variables"
                    | "constants"
                    | "ancestors"
                    | "superclass"
            )
            || self.find_class_method(cls, name).is_some()
            || self.find_class_define_method(cls, name).is_some()
            || {
                let sclass = format!("#<Class:{cls}>");
                self.find_define_method(&sclass, name).is_some()
                    || self.find_method_owner(&sclass, name).is_some()
            }
            || self.find_method("Class", name).is_some()
            || self.find_method("Module", name).is_some()
    }
    pub fn responds_to(&self, name: &str) -> bool {
        let this = self.current_self();
        if let Some(cls) = self.classref_name(&this) {
            return self.class_responds_to(&cls, name);
        }
        if let Some(cls) = self.object_class(&this) {
            if self.find_method(&cls, name).is_some()
                || self.find_define_method(&cls, name).is_some()
            {
                return true;
            }
            // A Struct member accessor (`x` / `x=`) is handled at dispatch, not as
            // a stored method, so a bare self-call to it must still resolve.
            if let Some((members, _)) = self.struct_def(&cls) {
                let member = name.strip_suffix('=').unwrap_or(name);
                if members.iter().any(|m| m == member) {
                    return true;
                }
            }
        }
        // A per-object singleton (`def obj.m`, `class << obj`) or a
        // `define_singleton_method` block on the current self.
        if self.find_singleton_method(&this, name).is_some()
            || self.find_singleton_define_method(&this, name).is_some()
        {
            return true;
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
    /// DAP: number of active frames (call depth), for step-over/out granularity.
    pub fn frame_depth(&self) -> usize {
        self.frames.len()
    }
    /// DAP: record the source line currently executing in the top frame.
    pub fn set_cur_line(&mut self, line: u32) {
        if let Some(f) = self.frames.last_mut() {
            f.line = line;
        }
    }
    /// DAP: the call stack as (method-or-`"main"`, current line), innermost first.
    pub fn dbg_stack(&self) -> Vec<(String, u32)> {
        self.frames
            .iter()
            .rev()
            .map(|f| {
                (
                    f.scope.method_name.clone().unwrap_or_else(|| "main".into()),
                    f.line,
                )
            })
            .collect()
    }
    /// DAP: the innermost frame's locals as (name, inspect), skipping synthetic
    /// temporaries (`__…`).
    pub fn dbg_locals(&mut self) -> Vec<(String, String)> {
        let env = self.cur_env();
        let names: Vec<String> = env
            .lock()
            .unwrap()
            .vars
            .keys()
            .filter(|k| !k.starts_with("__"))
            .cloned()
            .collect();
        names
            .into_iter()
            .map(|n| {
                let v = self.get_local(&n);
                (n, self.inspect(&v))
            })
            .collect()
    }
    /// Read a local, walking the scope chain to enclosing environments.
    pub fn get_local(&self, name: &str) -> Value {
        let mut env = self.cur_env();
        loop {
            if let Some(v) = env.lock().unwrap().vars.get(name).cloned() {
                return v;
            }
            let parent = env.lock().unwrap().parent.clone();
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
            if env.lock().unwrap().vars.contains_key(name) {
                env.lock().unwrap().vars.insert(name.to_string(), v);
                return;
            }
            let parent = env.lock().unwrap().parent.clone();
            match parent {
                Some(p) => env = p,
                None => break,
            }
        }
        self.cur_env()
            .lock()
            .unwrap()
            .vars
            .insert(name.to_string(), v);
    }
    pub fn local_defined(&self, name: &str) -> bool {
        let mut env = self.cur_env();
        loop {
            if env.lock().unwrap().vars.contains_key(name) {
                return true;
            }
            let parent = env.lock().unwrap().parent.clone();
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
    /// Whether `name` is a registered constant — true even when its value is nil
    /// (`Value::Undef`), which `get_const` cannot distinguish from unset. Lets a
    /// deliberately-nil constant (`File::ALT_SEPARATOR = nil`) read back as nil
    /// instead of raising `uninitialized constant`.
    pub fn has_const(&self, name: &str) -> bool {
        self.consts.contains_key(name)
    }
    pub fn set_const(&mut self, name: &str, v: Value) {
        self.consts.insert(name.to_string(), v);
    }
    /// `Module#remove_const` — remove the constant (and, if it named a class/
    /// module, its registration). Returns the previous value.
    pub fn remove_const(&mut self, name: &str) -> Value {
        let old = self.consts.shift_remove(name).unwrap_or(Value::Undef);
        self.classes.shift_remove(name);
        old
    }
    /// The names of user-defined constants in the flat store (`Module#constants`).
    pub fn const_names(&self) -> Vec<String> {
        self.consts.keys().cloned().collect()
    }
    /// Register `autoload name, path`: a lazy `require path` fired the first time
    /// the fully-qualified constant `name` is read and found undefined.
    pub fn set_autoload(&mut self, name: &str, path: &str) {
        self.autoloads.insert(name.to_string(), path.to_string());
    }
    /// The pending autoload path for `name`, if any (`Module#autoload?`).
    pub fn autoload_path(&self, name: &str) -> Option<String> {
        self.autoloads.get(name).cloned()
    }
    /// Consume and return `name`'s autoload path, removing it so the require runs
    /// at most once even if the required file doesn't define the constant.
    pub fn take_autoload(&mut self, name: &str) -> Option<String> {
        self.autoloads.shift_remove(name)
    }
    // Instance vars live on the current `self` object; at the top level (self is
    // the main object) they fall back to a global-keyed table.
    pub fn get_ivar(&self, name: &str) -> Value {
        match self.current_self() {
            Value::Obj(_) => {
                match self.obj(&self.current_self()) {
                    Some(RObj::Object { ivars, .. }) => {
                        ivars.get(name).cloned().unwrap_or(Value::Undef)
                    }
                    // `@x` where `self` is a class/module (class-level ivar).
                    Some(RObj::ClassRef(cls)) => self
                        .class_ivars
                        .get(cls)
                        .and_then(|m| m.get(name))
                        .cloned()
                        .unwrap_or(Value::Undef),
                    _ => Value::Undef,
                }
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
            Value::Obj(i) => match self.heap.get_mut(i as usize) {
                Some(RObj::Object { ivars, .. }) => {
                    ivars.insert(name.to_string(), v);
                }
                // `@x = v` where `self` is a class/module (class-level ivar).
                Some(RObj::ClassRef(cls)) => {
                    let cls = cls.clone();
                    self.class_ivars
                        .entry(cls)
                        .or_default()
                        .insert(name.to_string(), v);
                }
                _ => {}
            },
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
    /// Register a runtime attribute accessor `field` on `class` (a reader and/or
    /// a writer), checked natively in dispatch as an `@field` get/set.
    pub fn add_attr(&mut self, class: &str, field: &str, reader: bool, writer: bool) {
        let e = self
            .attr_accessors
            .entry(class.to_string())
            .or_default()
            .entry(field.to_string())
            .or_insert((false, false));
        e.0 |= reader;
        e.1 |= writer;
    }
    /// If `method` is a runtime attribute accessor on `class` or an ancestor,
    /// return `(field, is_writer)`; a trailing `=` selects the writer.
    pub fn attr_access(&self, class: &str, method: &str) -> Option<(String, bool)> {
        let (field, writer) = match method.strip_suffix('=') {
            Some(f) => (f, true),
            None => (method, false),
        };
        let mut cur = Some(class.to_string());
        while let Some(c) = cur {
            if let Some((r, w)) = self.attr_accessors.get(&c).and_then(|m| m.get(field)) {
                if (writer && *w) || (!writer && *r) {
                    return Some((field.to_string(), writer));
                }
            }
            cur = self
                .classes
                .get(&c)
                .and_then(|d| d.superclass.clone())
                .map(|s| self.resolve_class_alias(&s, &c));
        }
        None
    }
    /// Runtime `Class#include`/`prepend`/`extend`: append `module` to the class's
    /// mixin list (deduped), creating the ClassDef if needed. `kind` is
    /// `"include"`, `"prepend"`, or `"extend"`.
    pub fn class_mixin(&mut self, class: &str, module: &str, kind: &str) {
        let def = self.classes.entry(class.to_string()).or_default();
        let list = match kind {
            "prepend" => &mut def.prepends,
            "extend" => &mut def.extends,
            _ => &mut def.includes,
        };
        let m = module.to_string();
        if !list.contains(&m) {
            list.push(m);
        }
    }
    pub fn class_exists(&self, name: &str) -> bool {
        self.classes.contains_key(name)
    }
    /// `undef`/`undef_method`/`remove_method` — drop the class's own instance
    /// method `name`. Inherited definitions are left intact (a full `undef` would
    /// install a shadowing tombstone; removing the own method is enough for the
    /// load-time uses gems make of it).
    pub fn remove_instance_method(&mut self, cls: &str, name: &str) {
        if let Some(def) = self.classes.get_mut(cls) {
            def.methods.shift_remove(name);
        }
    }
    /// Register an anonymous class/module (`Class.new`/`Module.new`) under a fresh
    /// name and return it. The optional superclass seeds the `ClassDef`; the block
    /// body (if any) is run afterwards as a `class_eval` by the caller.
    pub fn define_anon_class(&mut self, superclass: Option<String>) -> String {
        self.struct_counter += 1;
        let name = format!("#<Class:{}>", self.struct_counter);
        self.classes.insert(
            name.clone(),
            ClassDef {
                superclass,
                ..ClassDef::default()
            },
        );
        name
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
    /// `Data.define(:x, :y)` — an immutable value class. Reuses the struct member
    /// store (so accessors / `to_h` / `==` / `members` / Enumerable come for
    /// free), but is tagged as `Data` so instances are frozen, the constructor
    /// accepts positional *or* keyword args, `with` is available, and `inspect`
    /// uses the `#<data …>` form.
    pub fn define_data(&mut self, members: Vec<String>) -> String {
        self.struct_counter += 1;
        let name = format!("Struct:{}", self.struct_counter);
        self.struct_defs.insert(name.clone(), (members, false));
        self.data_classes.insert(name.clone());
        name
    }
    /// Whether `name` is a `Data.define`d class (vs a plain `Struct`).
    pub fn is_data_class(&self, name: &str) -> bool {
        self.data_classes.contains(name)
    }
    /// The class name a class variable read/write resolves against, given `self`:
    /// an instance's class, or a class-reference's own name.
    pub fn cvar_owner(&self, this: &Value) -> Option<String> {
        self.object_class(this).or_else(|| self.classref_name(this))
    }
    /// Fetch a compiled method body previously registered under `name` (used to
    /// retrieve the body of a runtime `def` by its synthetic retrieval name).
    pub fn method_def(&self, name: &str) -> Option<MethodDef> {
        self.methods.get(name).cloned()
    }
    /// `obj.extend(M)` — mix module `M`'s instance methods into `obj`'s singleton
    /// method table so they answer on just this one object (MRI: extend inserts
    /// the module into the object's singleton ancestry). Compiled `def`s and
    /// `define_method` blocks are copied, following `M`'s own `include` chain.
    pub fn extend_object(&mut self, id: u32, module: &str) {
        // Collect the module plus the modules it includes (shallow BFS).
        let mut mods = vec![module.to_string()];
        let mut i = 0;
        while i < mods.len() {
            if let Some(cd) = self.classes.get(&mods[i]) {
                for inc in cd.includes.clone() {
                    if !mods.contains(&inc) {
                        mods.push(inc);
                    }
                }
            }
            i += 1;
        }
        for mname in &mods {
            if let Some(cd) = self.classes.get(mname).cloned() {
                for (n, def) in cd.methods {
                    self.add_singleton_method(id, &n, def);
                }
            }
            if let Some(dm) = self.define_methods.get(mname).cloned() {
                for (n, proc) in dm {
                    self.add_singleton_define_method(id, &n, proc);
                }
            }
        }
    }

    /// Register a per-object singleton method (`def obj.m`, `class << obj`).
    pub fn add_singleton_method(&mut self, id: u32, name: &str, def: MethodDef) {
        self.singleton_methods
            .entry(id)
            .or_default()
            .insert(name.to_string(), def);
    }
    /// A per-object singleton method for `name`, if `v` is an object that has one.
    pub fn find_singleton_method(&self, v: &Value, name: &str) -> Option<MethodDef> {
        if self.singleton_methods.is_empty() {
            return None;
        }
        match v {
            Value::Obj(id) => self
                .singleton_methods
                .get(id)
                .and_then(|m| m.get(name))
                .cloned(),
            _ => None,
        }
    }
    /// Register a `define_singleton_method` (a block Proc) on a specific object.
    pub fn add_singleton_define_method(&mut self, id: u32, name: &str, proc: Value) {
        self.singleton_define_methods
            .entry(id)
            .or_default()
            .insert(name.to_string(), proc);
    }
    /// A per-object `define_singleton_method` block for `name`, if `v` has one.
    pub fn find_singleton_define_method(&self, v: &Value, name: &str) -> Option<Value> {
        if self.singleton_define_methods.is_empty() {
            return None;
        }
        match v {
            Value::Obj(id) => self
                .singleton_define_methods
                .get(id)
                .and_then(|m| m.get(name))
                .cloned(),
            _ => None,
        }
    }
    /// Register a `Klass.define_singleton_method` block as a class method (proc-
    /// backed, inherited by subclasses).
    pub fn add_class_define_method(&mut self, class: &str, name: &str, proc: Value) {
        self.class_define_methods
            .entry(class.to_string())
            .or_default()
            .insert(name.to_string(), proc);
    }
    /// A `Klass.define_singleton_method` block for `name`, walking the superclass
    /// chain (a class-level singleton method is inherited like any class method).
    pub fn find_class_define_method(&self, class: &str, name: &str) -> Option<Value> {
        if self.class_define_methods.is_empty() {
            return None;
        }
        let mut cur = Some(class.to_string());
        let mut guard = 0;
        while let Some(c) = cur {
            if let Some(p) = self.class_define_methods.get(&c).and_then(|m| m.get(name)) {
                return Some(p.clone());
            }
            guard += 1;
            if guard > 100 {
                break;
            }
            cur = self.superclass_of(&c);
        }
        None
    }
    /// Register a class method (`def self.m` equivalent) on a class at runtime
    /// (`def Klass.m`, `Klass.instance_eval { def m }`).
    pub fn add_class_method(&mut self, class: &str, name: &str, def: MethodDef) {
        self.classes
            .entry(class.to_string())
            .or_default()
            .class_methods
            .insert(name.to_string(), def);
    }
    /// Register an instance method on a class at runtime (`class_eval { def m }`).
    pub fn add_instance_method(&mut self, class: &str, name: &str, def: MethodDef) {
        self.classes
            .entry(class.to_string())
            .or_default()
            .methods
            .insert(name.to_string(), def);
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
    /// Bare names (no `@@`) of every class variable visible from `class_name`,
    /// walking the superclass chain (`Module#class_variables`).
    pub fn class_var_names(&self, class_name: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut cur = Some(class_name.to_string());
        while let Some(c) = cur {
            if let Some(m) = self.class_vars.get(&c) {
                for k in m.keys() {
                    if !names.contains(k) {
                        names.push(k.clone());
                    }
                }
            }
            cur = self.superclass_of(&c);
        }
        names
    }
    /// Whether `var` (bare, no `@@`) is defined on `class_name` or an ancestor.
    pub fn cvar_defined(&self, class_name: &str, var: &str) -> bool {
        let mut cur = Some(class_name.to_string());
        while let Some(c) = cur {
            if self.class_vars.get(&c).is_some_and(|m| m.contains_key(var)) {
                return true;
            }
            cur = self.superclass_of(&c);
        }
        false
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
        if self.data_classes.remove(old) {
            self.data_classes.insert(new.to_string());
        }
    }
    /// Re-register an anonymous class/module (`Class.new`/`Module.new`) under the
    /// constant it is first assigned to, so `Foo = Class.new` names it `Foo`
    /// (matching MRI) and `include Foo` (resolved by name) finds it. Also moves any
    /// class variables / class-level ivars keyed by the old anonymous name.
    pub fn is_anon_class(&self, name: &str) -> bool {
        name.starts_with("#<Class:") && self.classes.contains_key(name)
    }
    pub fn rename_class(&mut self, old: &str, new: &str) {
        if let Some(def) = self.classes.shift_remove(old) {
            self.classes.insert(new.to_string(), def);
        }
        if let Some(v) = self.class_vars.shift_remove(old) {
            self.class_vars.insert(new.to_string(), v);
        }
        if let Some(v) = self.class_ivars.shift_remove(old) {
            self.class_ivars.insert(new.to_string(), v);
        }
        if let Some(v) = self.define_methods.shift_remove(old) {
            self.define_methods.insert(new.to_string(), v);
        }
        if let Some(v) = self.class_define_methods.shift_remove(old) {
            self.class_define_methods.insert(new.to_string(), v);
        }
        if let Some(v) = self.method_aliases.shift_remove(old) {
            self.method_aliases.insert(new.to_string(), v);
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
        self.classes
            .get(name)
            .and_then(|d| d.superclass.clone())
            .map(|s| self.resolve_class_alias(&s, name))
    }
    /// Resolve a class name that may be a constant *alias* to a class value
    /// (`Alias = Base; class C < Alias`) to the real class it refers to. `class
    /// C < expr` stores `expr` as a static name, so an aliased superclass would
    /// otherwise name a non-class constant and break the ancestry chain. A name
    /// that is already a registered class is returned unchanged.
    /// Resolve a superclass name to the actual registered class. `from` is the
    /// class whose superclass is being resolved, so a suffix match that would
    /// point back at `from` (a self-inheritance cycle) is skipped — the common
    /// `class Ns::X < ::X` pattern (`ActiveSupport::Logger < ::Logger`,
    /// `I18n::ArgumentError < ::ArgumentError`) must resolve `X` to the top-level
    /// class, not the nested one being defined.
    pub fn resolve_class_alias(&self, name: &str, from: &str) -> String {
        // Already the fully-qualified registered class — use as-is.
        if self.classes.contains_key(name) {
            return name.to_string();
        }
        // A builtin class/exception name refers to the top-level builtin, never a
        // nested user class that merely shares the suffix.
        if self.is_builtin_class(name) || is_builtin_exception_name(name) {
            return name.to_string();
        }
        // Prefer the lexically-nearest class: walk `from`'s enclosing namespaces
        // and return the first registered `<namespace>::<name>`. This resolves a
        // bare superclass to the copy in the innermost enclosing scope — e.g. a
        // per-subclass nested class an `inherited` hook created at runtime
        // (`class Capture < NodeTranslator` inside `Compiler` → the runtime-made
        // `Compiler::NodeTranslator`, not the base one) — mirroring Ruby's lexical
        // constant lookup for the superclass, instead of an arbitrary suffix match.
        if !name.contains("::") {
            let mut prefix = from;
            while let Some(idx) = prefix.rfind("::") {
                prefix = &prefix[..idx];
                let cand = format!("{prefix}::{name}");
                if cand != from && self.classes.contains_key(&cand) {
                    return cand;
                }
            }
        }
        // A partial or short nested-class name that names a class registered
        // under its *fully-qualified* form. `class C < Foo::Bar` inside another
        // module captures `Foo::Bar`, but compile-time resolution only qualifies
        // names already registered, so forward/runtime references stay partial
        // (concurrent-ruby: `Concurrent::Delay` super `Synchronization::Lockable-
        // Object`, real class `Concurrent::Synchronization::LockableObject`).
        // Match the registered class whose qualified name ends with `::name`,
        // never `from` itself.
        let suffix = format!("::{name}");
        if let Some(full) = self
            .classes
            .keys()
            .find(|k| k.as_str() != from && k.ends_with(&suffix))
        {
            return full.clone();
        }
        // A constant holding a class — an alias (`Alias = Base`) or a runtime-
        // selected implementation (`Impl = case … end`). Resolve recursively so a
        // short name the constant points to is itself fully qualified.
        let c = self.get_const(name);
        if let Some(RObj::ClassRef(real)) = self.obj(&c) {
            if real != name && real != from {
                return self.resolve_class_alias(real, from);
            }
        }
        if !name.contains("::") {
            for (k, v) in &self.consts {
                if k.ends_with(&suffix) {
                    if let Some(RObj::ClassRef(real)) = self.obj(v) {
                        if real != name && real != from {
                            return self.resolve_class_alias(real, from);
                        }
                    }
                }
            }
        }
        name.to_string()
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
            cur = def.superclass.clone().map(|s| self.resolve_class_alias(&s, &name));
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
        // `Class < Module` in MRI: a class reference is both a Class and a Module.
        if class == "Module" && actual == "Class" {
            return true;
        }
        if class == "Comparable" && matches!(actual.as_str(), "Integer" | "Float" | "String") {
            return true;
        }
        if class == "Enumerable" && matches!(actual.as_str(), "Array" | "Hash" | "Range") {
            return true;
        }
        if actual == "DateTime" && matches!(class, "Date" | "Comparable") {
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
            "DateTime" => own(&["Date", "Comparable"]),
            "Array" | "Hash" | "Range" | "Set" | "Struct" => own(&["Enumerable"]),
            _ => {
                if self.classes.contains_key(name) {
                    // A user-defined class: self, its included modules, then up
                    // the superclass chain; finally the common root.
                    let mut out = Vec::new();
                    let mut cur = Some(name.to_string());
                    while let Some(n) = cur {
                        // Prepended modules precede the class in the chain.
                        if let Some(def) = self.classes.get(&n) {
                            for m in def.prepends.iter().rev() {
                                out.push(m.clone());
                            }
                        }
                        out.push(n.clone());
                        match self.classes.get(&n) {
                            Some(def) => {
                                for m in def.includes.iter().rev() {
                                    out.push(m.clone());
                                }
                                cur = def.superclass.clone().map(|s| self.resolve_class_alias(&s, &n));
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
                | "BigDecimal"
                | "String"
                | "Symbol"
                | "Array"
                | "Hash"
                | "Range"
                | "Proc"
                | "Method"
                | "Object"
                | "BasicObject"
                | "Module"
                | "Class"
                | "Kernel"
                | "Comparable"
                | "Enumerable"
                | "NilClass"
                | "TrueClass"
                | "FalseClass"
                | "Set"
                | "Struct"
                | "Data"
                | "Enumerator"
                | "Time"
                | "Date"
                | "DateTime"
                | "Math"
                | "JSON"
                | "ERB"
                | "Fiber"
                | "Thread"
                | "Mutex"
                | "Thread::Mutex"
                | "Monitor"
                | "Queue"
                | "Thread::Queue"
                | "SizedQueue"
                | "Thread::SizedQueue"
                | "ConditionVariable"
                | "Thread::ConditionVariable"
                | "File"
                | "Dir"
                | "IO"
                | "TCPServer"
                | "TCPSocket"
                | "SecureRandom"
                | "Base64"
                | "Digest"
                | "Digest::MD5"
                | "Digest::SHA1"
                | "Digest::SHA256"
                | "OpenStruct"
                | "SQLite3"
                | "SQLite3::Database"
                | "Fiddle"
                | "Fiddle::Handle"
                | "Fiddle::Function"
                | "Fiddle::Pointer"
                | "StringIO"
                | "Random"
                | "Regexp"
                | "MatchData"
                | "Encoding"
                | "Ractor"
                | "Etc"
                | "CGI"
                | "Timeout"
                // `GC`/`ObjectSpace` are modeled as class-refs so their module
                // methods dispatch through `dispatch_classref` (GC control is a
                // no-op here; ObjectSpace's heap enumeration is limited).
                | "GC"
                | "ObjectSpace"
                // `Zlib` and its stream classes — DEFLATE/zlib/gzip via `flate2`,
                // dispatched through `dispatch_classref`.
                | "Zlib"
                | "Zlib::Deflate"
                | "Zlib::Inflate"
                | "Zlib::GzipWriter"
                | "Zlib::GzipReader"
                // `ENV` is modeled as a class-ref so its `[]`/`fetch`/… dispatch
                // through `dispatch_classref` (it is the process environment).
                | "ENV"
        )
    }
    pub fn classref_name(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::ClassRef(n)) => Some(n.clone()),
            _ => None,
        }
    }
    /// The `#inspect` string for an IO/File handle (id into `io_handles`):
    /// `#<IO:<STDOUT>>` for the standard streams, `#<File:/path>` for an open
    /// file, `#<File:/path (closed)>` once closed. Matches MRI's `IO#inspect`.
    fn io_inspect_str(&self, id: u32) -> String {
        match self.io_handles.get(id as usize) {
            Some(IoCell::Stdout) => "#<IO:<STDOUT>>".to_string(),
            Some(IoCell::Stderr) => "#<IO:<STDERR>>".to_string(),
            Some(IoCell::Stdin) => "#<IO:<STDIN>>".to_string(),
            Some(IoCell::File { file, path }) => {
                if file.is_some() {
                    format!("#<File:{path}>")
                } else {
                    format!("#<File:{path} (closed)>")
                }
            }
            Some(IoCell::TcpListener { listener, local }) => {
                if listener.is_some() {
                    format!("#<TCPServer:{local}>")
                } else {
                    "#<TCPServer: (closed)>".to_string()
                }
            }
            Some(IoCell::TcpStream { stream, peer, .. }) => {
                if stream.is_some() {
                    format!("#<TCPSocket:{peer}>")
                } else {
                    "#<TCPSocket: (closed)>".to_string()
                }
            }
            None => "#<IO:(invalid)>".to_string(),
        }
    }
    /// Look up `method` on `class`, walking the ancestor chain (own methods,
    /// then included modules, then the superclass), returning the method and the
    /// class/module it was defined in.
    /// Search a module (and its transitively prepended/included modules) for an
    /// instance method, resolving a partial module name to its registered class.
    /// This is what makes module-including-module work: a class that includes `A`
    /// finds `B`'s methods when `A` includes `B` (concurrent-ruby's `Obligation`
    /// includes `Dereferenceable`). Returns the method and its owning module.
    fn find_in_module(&self, module: &str, method: &str) -> Option<(MethodDef, String)> {
        let name = if self.classes.contains_key(module) {
            module.to_string()
        } else {
            self.resolve_class_alias(module, "")
        };
        let def = self.classes.get(&name)?;
        for p in def.prepends.iter().rev() {
            if let Some(r) = self.find_in_module(p, method) {
                return Some(r);
            }
        }
        if let Some(m) = def.methods.get(method) {
            return Some((m.clone(), name.clone()));
        }
        if let Some(target) = self.method_aliases.get(&name).and_then(|a| a.get(method)) {
            let target = target.clone();
            if target != method {
                if let Some(r) = self.find_in_module(&name, &target) {
                    return Some(r);
                }
            }
        }
        for i in def.includes.iter().rev() {
            if let Some(r) = self.find_in_module(i, method) {
                return Some(r);
            }
        }
        None
    }
    pub fn find_method_owner(&self, class: &str, method: &str) -> Option<(MethodDef, String)> {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let def = self.classes.get(&name)?;
            // Prepended modules sit ahead of the class's own methods (last
            // prepend wins, matching Ruby's reverse-order ancestor insertion).
            for module in def.prepends.iter().rev() {
                if let Some(r) = self.find_in_module(module, method) {
                    return Some(r);
                }
            }
            if let Some(m) = def.methods.get(method) {
                return Some((m.clone(), name.clone()));
            }
            // An `alias`/`alias_method` on this class resolves to its target
            // method (e.g. activesupport's `alias :cattr_accessor :mattr_accessor`
            // on `Module`). Resolve the target from this class onward.
            if let Some(target) = self.method_aliases.get(&name).and_then(|m| m.get(method)) {
                let target = target.clone();
                if let Some(found) = self.find_method_owner(&name, &target) {
                    return Some(found);
                }
            }
            // Included modules (transitively) take priority over the superclass
            // (last include wins, matching Ruby's reverse-order ancestor insertion).
            for module in def.includes.iter().rev() {
                if let Some(r) = self.find_in_module(module, method) {
                    return Some(r);
                }
            }
            cur = def.superclass.clone().map(|s| self.resolve_class_alias(&s, &name));
        }
        None
    }
    /// Look up `method` on `class`, walking the ancestor chain.
    pub fn find_method(&self, class: &str, method: &str) -> Option<MethodDef> {
        self.find_method_owner(class, method).map(|(m, _)| m)
    }
    /// Enumerate the instance-method names of a class for reflection
    /// (`Module#instance_methods`, `#public_instance_methods`). With
    /// `inherited == false`, only the class's own methods; with `true`, own plus
    /// every *user-defined* ancestor (included modules and superclasses) walked
    /// via `class_ancestry`. Builtin ancestors (`Object`/`Kernel`/`Comparable`/…)
    /// are not enumerable here, so the `true` set is bounded to the user-defined
    /// portion of the chain — it does not include MRI's builtin Kernel methods.
    /// `define_method`-created methods are included; synthetic/internal names
    /// (`__class_body__` and anything starting with `__`) are excluded. Names are
    /// deduplicated, keeping the first (nearest) occurrence.
    pub fn instance_method_names(&self, class: &str, inherited: bool) -> Vec<String> {
        let chain: Vec<String> = if inherited {
            self.class_ancestry(class)
        } else {
            vec![class.to_string()]
        };
        let mut out = Vec::new();
        for n in &chain {
            if let Some(def) = self.classes.get(n) {
                for k in def.methods.keys() {
                    if !k.starts_with("__") {
                        out.push(k.clone());
                    }
                }
            }
            if let Some(dm) = self.define_methods.get(n) {
                for k in dm.keys() {
                    if !k.starts_with("__") {
                        out.push(k.clone());
                    }
                }
            }
        }
        dedup_keep_first(out)
    }
    /// `Module#method_defined?` — whether `method` is defined on `class` or any
    /// ancestor (own methods, included modules, superclasses, and
    /// `define_method`-created methods). Visibility is not modeled, so this also
    /// serves `public_method_defined?`.
    pub fn is_method_defined(&self, class: &str, method: &str) -> bool {
        self.find_method_owner(class, method).is_some()
            || self.find_define_method(class, method).is_some()
    }
    /// Resolve a `super` call: find `method` in the receiver's linearized
    /// ancestry *after* the position of `def_class` (the current method's
    /// owner). Walking the receiver's full ancestry — not just `def_class`'s
    /// superclass — is what makes `prepend`/`include` super reach the class
    /// method that follows in `Module#ancestors` order.
    pub fn find_super(
        &self,
        recv_class: &str,
        def_class: &str,
        method: &str,
    ) -> Option<(MethodDef, String)> {
        let anc = self.class_ancestry(recv_class);
        let start = anc
            .iter()
            .position(|c| c == def_class)
            .map(|i| i + 1)
            .unwrap_or(0);
        for name in anc.iter().skip(start) {
            if let Some(def) = self.classes.get(name) {
                if let Some(m) = def.methods.get(method) {
                    return Some((m.clone(), name.clone()));
                }
            }
        }
        None
    }
    /// Like `find_class_method`, but also returns the class that actually owns
    /// the resolved method (needed as the `def_class` so `super` resumes above
    /// the defining class, not the lookup-origin subclass).
    pub fn find_class_method_owner(
        &self,
        class: &str,
        method: &str,
    ) -> Option<(MethodDef, String)> {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let def = self.classes.get(&name)?;
            if let Some(m) = def.class_methods.get(method) {
                return Some((m.clone(), name.clone()));
            }
            for module in def.extends.iter().rev() {
                let nested = format!("{name}::{module}");
                for cand in [nested.as_str(), module] {
                    if self.classes.contains_key(cand) {
                        // Resolve through the module's own aliases and includes.
                        if let Some((m, _)) = self.find_in_module(cand, method) {
                            return Some((m, name.clone()));
                        }
                    }
                }
            }
            cur = def.superclass.clone().map(|s| self.resolve_class_alias(&s, &name));
        }
        None
    }
    /// A class method (`def self.m`), walking the superclass chain.
    pub fn find_class_method(&self, class: &str, method: &str) -> Option<MethodDef> {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let def = self.classes.get(&name)?;
            if let Some(m) = def.class_methods.get(method) {
                return Some(m.clone());
            }
            // `extend M` adds M's *instance* methods as class methods, after
            // the class's own `def self.m` (last extend wins). A bare `M` written
            // inside this class's body may name a sibling nested module
            // (`<name>::M`) — which registers at runtime, so it wasn't resolvable
            // when the extend was compiled — so try the class's own namespace
            // first, then the stored/top-level name.
            for module in def.extends.iter().rev() {
                let nested = format!("{name}::{module}");
                for cand in [nested.as_str(), module] {
                    if self.classes.contains_key(cand) {
                        if let Some((m, _)) = self.find_in_module(cand, method) {
                            return Some(m);
                        }
                    }
                }
            }
            cur = def.superclass.clone().map(|s| self.resolve_class_alias(&s, &name));
        }
        None
    }
    /// `super` from a singleton/class method (`def self.m`): resume class-method
    /// lookup in the singleton-class ancestry above `def_class`, i.e. starting at
    /// `def_class`'s superclass. Returns the class method and its owner class.
    pub fn find_super_class_method(
        &self,
        def_class: &str,
        method: &str,
    ) -> Option<(MethodDef, String)> {
        let mut cur = self.superclass_of(def_class);
        while let Some(name) = cur {
            let def = self.classes.get(&name)?;
            if let Some(m) = def.class_methods.get(method) {
                return Some((m.clone(), name.clone()));
            }
            // `extend M` on an ancestor contributes M's instance methods as that
            // ancestor's class methods (after its own `def self.m`, last wins).
            for module in def.extends.iter().rev() {
                let nested = format!("{name}::{module}");
                for cand in [&nested, module] {
                    if let Some(md) = self.classes.get(cand) {
                        if let Some(m) = md.methods.get(method) {
                            return Some((m.clone(), name.clone()));
                        }
                    }
                }
            }
            cur = def.superclass.clone().map(|s| self.resolve_class_alias(&s, &name));
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
            // A Class/Module receiver: its instance variables are class-level
            // ivars, stored in `class_ivars` (mirrors `get_ivar`). Reflective
            // `instance_variable_get` must read the same store bare `@x` uses.
            Some(RObj::ClassRef(cls)) => self
                .class_ivars
                .get(cls)
                .and_then(|m| m.get(name))
                .cloned()
                .unwrap_or(Value::Undef),
            _ => Value::Undef,
        }
    }
    /// Set the instance variable `name` (bare, no `@`) on a specific object.
    pub fn set_ivar_of(&mut self, obj: &Value, name: &str, v: Value) {
        // A Class/Module receiver writes into `class_ivars` (mirrors `set_ivar`),
        // so reflective `instance_variable_set` and `class_eval { @x = … }` land
        // where `ivar_of`/bare `@x` read. Resolve the class name first so the
        // immutable `obj` borrow ends before the mutable store access.
        let cls = match self.obj(obj) {
            Some(RObj::ClassRef(cls)) => Some(cls.clone()),
            _ => None,
        };
        if let Some(cls) = cls {
            self.class_ivars
                .entry(cls)
                .or_default()
                .insert(name.to_string(), v);
            return;
        }
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
            Some(RObj::ClassRef(cls)) => self
                .class_ivars
                .get(cls)
                .map(|m| m.keys().map(|k| format!("@{k}")).collect())
                .unwrap_or_default(),
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
    /// The MRI context label for the innermost active frame: `'<main>'` at the top
    /// level, `'<DefClass>#<method>'` inside an instance method (an unqualified
    /// top-level `def` reports `Object#name`, matching MRI's `-e:1:in 'Object#f'`),
    /// and `'<DefClass>.<method>'` inside a class/singleton method (`def self.m`),
    /// matching MRI's `-e:1:in 'A.f'`.
    fn innermost_context(&self) -> String {
        match self.frames.last() {
            Some(f) => match &f.scope.method_name {
                Some(m) => {
                    let cls = f.scope.def_class.clone().unwrap_or_else(|| "Object".into());
                    // A class/singleton method's `self` is the class ref itself.
                    let sep = if matches!(self.obj(&f.scope.self_obj), Some(RObj::ClassRef(_))) {
                        '.'
                    } else {
                        '#'
                    };
                    format!("{cls}{sep}{m}")
                }
                None => "<main>".into(),
            },
            None => "<main>".into(),
        }
    }
    /// Append one MRI-format backtrace frame (`<src>:<line>:in '<ctx>'`) for the
    /// in-flight exception. Called from `abort` as an exception unwinds through
    /// each chunk boundary, so frames accumulate innermost-first (MRI's print
    /// order). Stored in a side table keyed by the exception's heap id (not on the
    /// object), so `e.instance_variables`/inspect are unaffected and a
    /// `rescue`/re-raise still finds the trace. No-op when no exception is pending.
    pub fn record_backtrace_frame(&mut self, src: &str, line: u32) {
        let Some(Value::Obj(id)) = self.pending_exc else {
            return;
        };
        let ctx = self.innermost_context();
        self.exc_backtraces
            .entry(id)
            .or_default()
            .push(format!("{src}:{line}:in '{ctx}'"));
    }
    /// Tag a String heap object as ASCII-8BIT/BINARY (`String#b`,
    /// `force_encoding("BINARY")`).
    pub fn mark_binary_string(&mut self, v: &Value) {
        if let Value::Obj(id) = v {
            self.binary_strings.insert(*id);
        }
    }
    /// Clear a String's BINARY tag (`force_encoding("UTF-8")` and friends).
    pub fn unmark_binary_string(&mut self, v: &Value) {
        if let Value::Obj(id) = v {
            self.binary_strings.remove(id);
        }
    }
    /// Whether a String heap object is tagged ASCII-8BIT/BINARY.
    pub fn is_binary_string(&self, v: &Value) -> bool {
        matches!(v, Value::Obj(id) if self.binary_strings.contains(id))
    }
    /// Format the pending (uncaught) exception in MRI's shape:
    /// `<src>:<line>:in '<ctx>': <msg> (<Class>)` followed by tab-indented
    /// `from <src>:<line>:in '<ctx>'` lines for the remaining frames. Returns
    /// `None` when no exception is pending. Consumes the pending exception.
    pub fn format_uncaught(&mut self) -> Option<String> {
        let exc = self.pending_exc.take()?;
        let class = self.class_of(&exc).to_string();
        let msg = match self.ivar_of(&exc, "message") {
            Value::Undef => class.clone(),
            m => self.to_s(&m),
        };
        let frames: Vec<String> = match &exc {
            Value::Obj(id) => self.exc_backtraces.get(id).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut out = match frames.split_first() {
            Some((first, _)) => format!("{first}: {msg} ({class})"),
            // No captured frame (e.g. an exception raised before any op ran):
            // fall back to the bare `<msg> (<Class>)` MRI still prints.
            None => format!("{msg} ({class})"),
        };
        for f in frames.iter().skip(1) {
            out.push('\n');
            out.push('\t');
            out.push_str("from ");
            out.push_str(f);
        }
        Some(out)
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
    /// Whether `class` is (or descends from) a builtin exception class. The
    /// builtin roots are name-based (`*Error`, `Exception`, `StopIteration`);
    /// user classes are resolved through the superclass chain.
    pub fn is_exception_class(&self, class: &str) -> bool {
        fn builtin(n: &str) -> bool {
            n.ends_with("Error") || n == "Exception" || n == "StopIteration"
        }
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            if builtin(&name) {
                return true;
            }
            cur = self.superclass_of(&name);
        }
        false
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
            cur = self
                .classes
                .get(&name)
                .and_then(|d| d.superclass.clone())
                .map(|s| self.resolve_class_alias(&s, &name));
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
                Some(RObj::DateTime { secs }) => self.datetime_to_s(secs),
                Some(RObj::Db { .. }) => "#<SQLite3::Database>".to_string(),
                // `Fiddle::Pointer#to_s` reads the pointed-to memory as a C
                // string (matching MRI); the library/function handles render a
                // short description.
                Some(RObj::FiddleHandle { id }) => format!("#<Fiddle::Handle id={id}>"),
                Some(RObj::FiddleFunc { addr, .. }) => {
                    format!("#<Fiddle::Function ptr=0x{addr:x}>")
                }
                Some(RObj::FiddlePtr { addr, size, .. }) => fiddle_read_cstr_or_len(addr, size),
                Some(RObj::Lazy { .. }) => "#<Enumerator::Lazy>".to_string(),
                Some(RObj::Enumerator { buf, method, .. }) => {
                    // MRI shows `#<Enumerator: <receiver>:<method>>`; we keep the
                    // materialized values in place of the receiver (they match for
                    // the common Array case) and append the producing method.
                    format!("#<Enumerator: {}:{method}>", self.inspect_array(&buf))
                }
                Some(RObj::Generator { .. }) => {
                    "#<Enumerator: #<Enumerator::Generator>:each>".to_string()
                }
                Some(RObj::Yielder { .. }) => "#<Enumerator::Yielder>".to_string(),
                Some(RObj::Fiber { .. }) => "#<Fiber (created)>".to_string(),
                Some(RObj::Thread { id }) => {
                    let alive = self
                        .threads
                        .get(id as usize)
                        .map(|t| !t.done.load(std::sync::atomic::Ordering::SeqCst))
                        .unwrap_or(false);
                    format!("#<Thread:{id:#x} {}>", if alive { "run" } else { "dead" })
                }
                Some(RObj::IoHandle { id }) => self.io_inspect_str(id),
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
                Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) | Some(RObj::CycleProc(_)) => {
                    "#<Proc>".to_string()
                }
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
                    // OpenStruct#to_s aliases inspect (`#<OpenStruct a=1, b=2>`).
                    // An Encoding object stringifies to its name (`"UTF-8"`).
                    if class == "Encoding" {
                        return match ivars.get("name") {
                            Some(n) => self.to_s(&n.clone()),
                            None => "#<Encoding>".to_string(),
                        };
                    }
                    if class == "OpenStruct" {
                        let body: Vec<String> = ivars
                            .iter()
                            .map(|(k, val)| format!("{k}={}", self.inspect(&val.clone())))
                            .collect();
                        return if body.is_empty() {
                            "#<OpenStruct>".to_string()
                        } else {
                            format!("#<OpenStruct {}>", body.join(", "))
                        };
                    }
                    if let Some((members, _)) = self.struct_def(&class) {
                        let parts: Vec<String> = members
                            .iter()
                            .map(|m| {
                                let v = ivars.get(m).cloned().unwrap_or(Value::Undef);
                                format!("{m}={}", self.inspect(&v))
                            })
                            .collect();
                        // `Data.define`d instances print `#<data …>`; Structs `#<struct …>`.
                        let kind = if self.is_data_class(&class) {
                            "data"
                        } else {
                            "struct"
                        };
                        format!("#<{kind} {class} {}>", parts.join(", "))
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
                Some(RObj::DateTime { secs }) => self.datetime_inspect(secs),
                Some(RObj::Db { .. }) => "#<SQLite3::Database>".to_string(),
                Some(RObj::FiddleHandle { id }) => format!("#<Fiddle::Handle id={id}>"),
                Some(RObj::FiddleFunc { addr, .. }) => {
                    format!("#<Fiddle::Function ptr=0x{addr:x}>")
                }
                // `Fiddle::Pointer#inspect` shows the address and known byte size
                // (MRI: `#<Fiddle::Pointer ptr=0x… size=N>`).
                Some(RObj::FiddlePtr { addr, size, .. }) => {
                    format!("#<Fiddle::Pointer ptr=0x{addr:x} size={size}>")
                }
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
                // `Encoding#inspect`: `#<Encoding:UTF-8>`. The binary encoding
                // inspects as `#<Encoding:BINARY (ASCII-8BIT)>` (MRI names the
                // object BINARY with its ASCII-8BIT alias in parens).
                Some(RObj::Object { class, ivars }) if class == "Encoding" => {
                    let name = ivars
                        .get("name")
                        .map(|n| self.to_s(&n.clone()))
                        .unwrap_or_default();
                    if name == "ASCII-8BIT" {
                        "#<Encoding:BINARY (ASCII-8BIT)>".to_string()
                    } else {
                        format!("#<Encoding:{name}>")
                    }
                }
                // `OpenStruct#inspect`: `#<OpenStruct a=1, b=2>` (ivars in order).
                Some(RObj::Object { class, ivars }) if class == "OpenStruct" => {
                    let body: Vec<String> = ivars
                        .iter()
                        .map(|(k, val)| format!("{k}={}", self.inspect(&val.clone())))
                        .collect();
                    if body.is_empty() {
                        "#<OpenStruct>".to_string()
                    } else {
                        format!("#<OpenStruct {}>", body.join(", "))
                    }
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
                Some(RObj::Generator { .. }) => "Enumerator",
                Some(RObj::Yielder { .. }) => "Enumerator::Yielder",
                Some(RObj::Fiber { .. }) => "Fiber",
                Some(RObj::Thread { .. }) => "Thread",
                Some(RObj::IoHandle { id }) => self
                    .io_handles
                    .get(*id as usize)
                    .map(|c| c.class_name())
                    .unwrap_or("IO"),
                Some(RObj::Time { .. }) => "Time",
                Some(RObj::Date { .. }) => "Date",
                Some(RObj::DateTime { .. }) => "DateTime",
                Some(RObj::Db { .. }) => "SQLite3::Database",
                Some(RObj::FiddleHandle { .. }) => "Fiddle::Handle",
                Some(RObj::FiddleFunc { .. }) => "Fiddle::Function",
                Some(RObj::FiddlePtr { .. }) => "Fiddle::Pointer",
                Some(RObj::Set(_)) => "Set",
                Some(RObj::Array(_)) => "Array",
                Some(RObj::Hash { .. }) => "Hash",
                Some(RObj::Symbol(_)) => "Symbol",
                Some(RObj::Range { .. }) => "Range",
                Some(RObj::FloatRange { .. }) => "Range",
                Some(RObj::StrRange { .. }) => "Range",
                Some(RObj::Proc { .. }) | Some(RObj::SymProc(_)) | Some(RObj::CycleProc(_)) => {
                    "Proc"
                }
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
        // Unary negation: the VM's `Negate` op forwards a heap number (BigInt or
        // Rational — the native Int/Float paths negate in-VM) here as
        // `(Neg, x, Undef)`. Preserve the operand's type.
        if matches!(op, Neg) {
            let rat = match self.obj(a) {
                Some(RObj::Rational(r)) => Some(r.clone()),
                _ => None,
            };
            if let Some(r) = rat {
                return Ok(self.new_rational(-r));
            }
            if let Some(x) = self.as_bigint(a) {
                return Ok(self.new_bigint(-x));
            }
            // `String#-@` returns a frozen copy of the string (Ruby's frozen-
            // string operator); a mutable string is duped and frozen, an already
            // frozen one returns itself.
            if self.as_str(a).is_some() && matches!(a, Value::Obj(_)) {
                let c = if self.is_frozen(a) {
                    a.clone()
                } else {
                    self.dup_value(a)
                };
                self.freeze_value(&c);
                return Ok(c);
            }
            return match a {
                Value::Int(n) => Ok(Value::Int(n.wrapping_neg())),
                Value::Float(f) => Ok(Value::Float(-*f)),
                _ => Err(format!("undefined method '-@' for {}", self.class_name(a))),
            };
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
                    // Ruby Rational#%: a - b*(a/b).floor (floored modulo).
                    Mod if !y.is_zero() => {
                        let q = (x.clone() / &y).floor();
                        Some(x.clone() - &y * q)
                    }
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
        // `DateTime` arithmetic (by day, like `Date`, but keeping the time of
        // day). `DateTime - DateTime` is the Rational number of days between
        // them; `DateTime ± Numeric` shifts by that many days and stays a
        // `DateTime`. Comparisons order by epoch seconds.
        if let Some(sa) = self.datetime_secs(a) {
            let num_f = |v: &Value| match v {
                Value::Int(n) => Some(*n as f64),
                Value::Float(f) => Some(*f),
                _ => None,
            };
            match op {
                Sub => {
                    if let Some(sb) = self.datetime_secs(b) {
                        let numer = num_bigint::BigInt::from((sa - sb).round() as i64);
                        let r =
                            num_rational::BigRational::new(numer, num_bigint::BigInt::from(86_400));
                        return Ok(self.new_rational(r));
                    }
                    if let Some(n) = num_f(b) {
                        return Ok(self.new_datetime(sa - n * 86_400.0));
                    }
                }
                Add => {
                    if let Some(n) = num_f(b) {
                        return Ok(self.new_datetime(sa + n * 86_400.0));
                    }
                }
                Lt | Gt | Le | Ge => {
                    if let Some(sb) = self.datetime_secs(b) {
                        return Ok(Value::Bool(cmp_ord(op, sa.total_cmp(&sb))));
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
                    // Two Encoding objects are equal when they name the same
                    // encoding — MRI's encodings are shared singletons, so `==`
                    // compares by identity; we carry a fresh object per call and
                    // compare by name to the same effect.
                    (
                        Some(RObj::Object {
                            class: ca,
                            ivars: ia,
                        }),
                        Some(RObj::Object {
                            class: cb,
                            ivars: ib,
                        }),
                    ) if ca == "Encoding" && cb == "Encoding" => {
                        match (ia.get("name"), ib.get("name")) {
                            (Some(x), Some(y)) => self.eq_values(x, y),
                            _ => false,
                        }
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
                    // Two DateTimes are equal when they name the same instant.
                    (Some(RObj::DateTime { secs: x }), Some(RObj::DateTime { secs: y })) => x == y,
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
                    // Two OpenStructs are equal when they carry the same
                    // attributes (name→value), order-independent like MRI.
                    (
                        Some(RObj::Object {
                            class: cx,
                            ivars: ix,
                        }),
                        Some(RObj::Object {
                            class: cy,
                            ivars: iy,
                        }),
                    ) if cx == "OpenStruct" && cy == "OpenStruct" => {
                        ix.len() == iy.len()
                            && ix
                                .iter()
                                .all(|(k, v)| iy.get(k).is_some_and(|w| self.eq_values(v, w)))
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
    // Ruby prints in scientific notation outside [1e-4, 1e15): a magnitude >= 1e15
    // has a decimal exponent past Float::DIG (15), and < 1e-4 is below -4.
    if a != 0.0 && !(1e-4..1e15).contains(&a) {
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

/// Every installed gem's `lib/` directory, so `require "gem"` resolves like
/// RubyGems (modern Ruby auto-activates gem libs onto $LOAD_PATH). Gem roots come
/// from `GEM_HOME`/`GEM_PATH` (colon-separated) when set, else rubylang's own gem
/// home `~/.rubylang` — rubylang is self-contained and does not read a system MRI
/// install (`gem install` writes into `~/.rubylang`, see `gem.rs`). Each root
/// holds `gems/<name>-<ver>/`, whose `lib/` (when present) goes on the load path.
/// Best-effort — unreadable dirs are skipped silently.
fn gem_lib_dirs() -> Vec<String> {
    use std::path::PathBuf;
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["GEM_HOME", "GEM_PATH"] {
        if let Ok(v) = std::env::var(var) {
            for p in v.split(':').filter(|s| !s.is_empty()) {
                roots.push(PathBuf::from(p));
            }
        }
    }
    if roots.is_empty() {
        // rubylang's own gem home: `~/.rubylang` holds `gems/` + `specifications/`
        // exactly like a RubyGems root, so the scan below reads it unchanged. No
        // MRI system path is consulted — the runtime is standalone.
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".rubylang"));
        }
    }
    let mut libs = Vec::new();
    for root in roots {
        let spec_dir = root.join("specifications");
        if let Ok(rd) = std::fs::read_dir(root.join("gems")) {
            for e in rd.flatten() {
                let gem_dir = e.path();
                // A gem's real require dirs come from its gemspec's `require_paths`
                // (usually `["lib"]`, but some — concurrent-ruby — use a custom
                // path like `lib/concurrent-ruby`). Fall back to `lib`.
                let spec = spec_dir.join(format!("{}.gemspec", e.file_name().to_string_lossy()));
                let paths = gemspec_require_paths(&spec).unwrap_or_else(|| vec!["lib".into()]);
                for p in paths {
                    let lib = gem_dir.join(&p);
                    if lib.is_dir() {
                        libs.push(lib.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    libs
}

/// Extract the quoted `require_paths` from a gemspec (`s.require_paths = ["lib",
/// "ext"]`), stripping `.freeze`. `None` if the file is unreadable or has no
/// `require_paths` line. A lightweight scan — gemspecs are Ruby, but this line is
/// a plain array literal in practice.
fn gemspec_require_paths(spec: &std::path::Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(spec).ok()?;
    let line = text.lines().find(|l| l.contains("require_paths"))?;
    let open = line.find('[')?;
    let close = line[open..].find(']')? + open;
    let inner = &line[open + 1..close];
    let paths: Vec<String> = inner
        .split(',')
        .filter_map(|part| {
            let p = part.trim().trim_end_matches(".freeze").trim();
            let p = p.trim_matches(|c| c == '"' || c == '\'');
            (!p.is_empty()).then(|| p.to_string())
        })
        .collect();
    (!paths.is_empty()).then_some(paths)
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

thread_local! {
    /// Set while `ruby --dap` is debugging: `run_chunk_on` then installs the DAP
    /// line-marker extension handler and runs the pure interpreter (no tracing
    /// JIT) so every `Op::Extended(DBG_LINE)` marker fires. Off for normal runs.
    static DEBUG_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Enable/disable DAP debug execution (installs the line-marker hook path).
pub fn set_debug_mode(on: bool) {
    DEBUG_MODE.with(|d| d.set(on));
}

/// Register every rubylang builtin + the numeric hook on a VM, then run it.
fn run_chunk_on(chunk: Chunk) -> Result<Value, String> {
    let mut vm = VM::new(chunk);
    crate::builtins::install(&mut vm);
    vm.set_numeric_hook(std::sync::Arc::new(|op, a, b| {
        crate::builtins::numeric_hook(op, a, b)
    }));
    if DEBUG_MODE.with(|d| d.get()) {
        // The DAP line marker pauses the interpreter; the tracing JIT would
        // compile hot loops and skip the markers, so it stays off in debug mode.
        vm.set_extension_handler(Box::new(|vm, id, _| {
            if id == ext::DBG_LINE {
                crate::dap::on_debug_line(vm);
            }
        }));
    } else {
        vm.enable_tracing_jit();
    }
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
    // An uncaught Ruby exception prints in MRI's shape (`<src>:<line>:in '<ctx>':
    // <msg> (<Class>)` + backtrace). `abort` captured each frame as the exception
    // unwound; format them here at the top-level boundary.
    if r.is_err() {
        if let Some(formatted) = with_host(|h| h.format_uncaught()) {
            return Err(formatted);
        }
    }
    r
}

/// Run a `require`/`load`d file's top-level chunk in its own fresh top-level
/// scope, so the required file's local variables neither leak into nor read from
/// the requiring file's locals (MRI evaluates a required file at the top-level
/// `main` binding, not the caller's). Constants, classes, methods, and globals
/// still persist on the shared host — they are not frame-local. A leftover
/// top-level control signal from the required file is cleared (its `return`/
/// `throw` just ends that file's evaluation).
pub fn run_required_main(chunk: Chunk) -> Result<Value, String> {
    let saved_active = with_host(|h| {
        h.frames.push(Frame {
            scope: Scope {
                locals: new_env(),
                block: None,
                self_obj: Value::Undef,
                method_name: None,
                def_class: None,
            },
            args: Vec::new(),
            line: 0,
        });
        h.active_scope.take()
    });
    let r = run_chunk_on(chunk);
    with_host(|h| {
        h.frames.pop();
        h.active_scope = saved_active;
        h.signal.take();
    });
    r
}

thread_local! {
    /// Nonzero while an advice handler is executing. Weaving is suppressed for
    /// the duration so a handler's own dispatch is never re-advised (prevents
    /// self-advising and infinite recursion).
    static IN_ADVICE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn in_advice() -> bool {
    IN_ADVICE.with(|f| f.get() > 0)
}

/// Fire one advice handler by name, propagating any raise. `args` is what the
/// handler receives (the call args for `before`, the result for `after`/`around`).
fn fire_advice(handler: &str, args: &[Value]) -> Result<Value, String> {
    IN_ADVICE.with(|f| f.set(f.get() + 1));
    let r = call_method(handler, args, None);
    IN_ADVICE.with(|f| f.set(f.get() - 1));
    r
}

/// Like `fire_advice`, but hands the handler a block (for true around-advice: the
/// block, when yielded, re-runs the intercepted method's original body).
fn fire_advice_block(handler: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    IN_ADVICE.with(|f| f.set(f.get() + 1));
    let r = call_method(handler, args, block);
    IN_ADVICE.with(|f| f.set(f.get() - 1));
    r
}

/// Run the around-advice chain for an intercepted call: each handler runs in
/// place of the body and receives the original args plus a block that runs the
/// next inner layer (another around handler, or finally the real body) once.
/// The outermost handler's return value is the call's result.
#[allow(clippy::too_many_arguments)]
fn run_around(
    handlers: &[String],
    def: &MethodDef,
    self_obj: &Value,
    args: &[Value],
    block: &Option<Value>,
    method_name: &Option<String>,
    def_class: &Option<String>,
) -> Result<Value, String> {
    let base = with_host(|h| h.around_len());
    let idx = with_host(|h| {
        h.push_around(
            handlers.to_vec(),
            def.clone(),
            self_obj.clone(),
            args.to_vec(),
            block.clone(),
            method_name.clone(),
            def_class.clone(),
        )
    });
    let r = drive_around(idx);
    with_host(|h| h.truncate_around(base));
    r
}

/// Advance one layer of an around weave. With handlers remaining, fire the next
/// one with a fresh native block bound to the rest; with none left, run the real
/// method body once (un-advised — `IN_ADVICE` is nonzero while a handler runs).
fn drive_around(idx: usize) -> Result<Value, String> {
    let call = with_host(|h| h.around_call(idx));
    match call.handlers.split_first() {
        None => run_method(
            &call.def,
            call.self_obj,
            &call.args,
            call.block,
            call.method_name,
            call.def_class,
        ),
        Some((handler, rest)) => {
            let child = with_host(|h| {
                h.push_around(
                    rest.to_vec(),
                    call.def.clone(),
                    call.self_obj.clone(),
                    call.args.clone(),
                    call.block.clone(),
                    call.method_name.clone(),
                    call.def_class.clone(),
                )
            });
            let blk = with_host(|h| h.new_around_block(child));
            fire_advice_block(handler, &call.args, Some(blk))
        }
    }
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
    // AOP weave. Fast path: `intercepts::any()` is an O(1) empty-check, so a call
    // with no registered advice pays only one bool test and takes `None`. The
    // `in_advice` guard keeps a handler's own calls from being advised.
    let advice: Option<Vec<(Advice, String)>> = if intercepts::any() && !in_advice() {
        method_name
            .as_deref()
            .map(intercepts::matches)
            .filter(|m| !m.is_empty())
    } else {
        None
    };
    if let Some(adv) = &advice {
        for (kind, handler) in adv {
            if *kind == Advice::Before {
                fire_advice(handler, args)?;
            }
        }
        // True around-advice: the handler runs INSTEAD of the body. It receives
        // the original call args and a block that, when yielded, runs the real
        // method body once — un-advised, because the IN_ADVICE guard set while a
        // handler runs suppresses re-weaving. The handler's return value is the
        // call's result whether or not it yielded (MRI around semantics). `after`
        // advice observes that final result.
        let arounds: Vec<String> = adv
            .iter()
            .filter(|(k, _)| *k == Advice::Around)
            .map(|(_, h)| h.clone())
            .collect();
        if !arounds.is_empty() {
            let val = run_around(
                &arounds,
                def,
                &self_obj,
                args,
                &block,
                &method_name,
                &def_class,
            )?;
            for (kind, handler) in adv {
                if *kind == Advice::After {
                    fire_advice(handler, std::slice::from_ref(&val))?;
                }
            }
            return Ok(val);
        }
    }
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
            line: 0,
        });
        // A method body runs against its own top frame, not any captured block
        // scope in effect at the call site.
        h.active_scope.take()
    });
    // Isolate the `def` target: a bare `def` inside an ordinary method body must
    // hoist normally even when called from within a `class_eval`/`instance_eval`.
    // Only touched when an eval is actually in flight (empty stack = no-op).
    let def_target_pushed = DEF_TARGET.with(|t| {
        let mut b = t.borrow_mut();
        if b.is_empty() {
            false
        } else {
            b.push(DefTarget::None);
            true
        }
    });
    let r = run_chunk_on(def.chunk.clone());
    if def_target_pushed {
        DEF_TARGET.with(|t| {
            t.borrow_mut().pop();
        });
    }
    let sig = with_host(|h| {
        h.frames.pop();
        h.active_scope = saved_active;
        h.signal.take()
    });
    let result = match sig {
        Some(Signal::Return(v)) => Ok(v),
        // A `throw` must keep unwinding past this method boundary to reach its
        // `catch`; re-arm the signal so the caller's chunk halts too.
        Some(other @ Signal::Throw(..)) => {
            with_host(|h| h.signal = Some(other));
            r
        }
        _ => r,
    };
    // AOP `after`/`around` weave: only on a normal (Ok) return, after the frame is
    // gone so handlers run at the call site's scope. `after` observes the result;
    // `around` replaces it (post-transform).
    let Some(adv) = advice else {
        return result;
    };
    match result {
        Ok(val) => {
            // `around` never reaches here (handled above, before the body ran);
            // only `after` observes the raw-body result.
            for (kind, handler) in &adv {
                if *kind == Advice::After {
                    fire_advice(handler, std::slice::from_ref(&val))?;
                }
            }
            Ok(val)
        }
        err => err,
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
    call_super_blk(explicit_args, None)
}

/// `super` with an optional block override. `block_override` is `Some` for
/// `super { … }` / `super(args) { … }` (a fresh block); `None` forwards the
/// current method's block.
pub fn call_super_blk(
    explicit_args: Option<Vec<Value>>,
    block_override: Option<Value>,
) -> Result<Value, String> {
    let (self_obj, method, def_class, cur_args) = with_host(|h| h.super_context());
    let (Some(method), Some(def_class)) = (method, def_class) else {
        return Err("super called outside of a method".to_string());
    };
    // `super` from a singleton/class method (`def self.m`): the receiver is a
    // class ref with no object class, so resolve through the singleton-class
    // ancestry (class methods above `def_class`) rather than the instance chain.
    if with_host(|h| h.object_class(&self_obj)).is_none() {
        if let Some(cls) = with_host(|h| h.classref_name(&self_obj)) {
            if let Some((def, owner)) =
                with_host(|h| h.find_super_class_method(&def_class, &method))
            {
                let args = explicit_args.unwrap_or(cur_args);
                let block = match block_override {
                    Some(b) => Some(b),
                    None => with_host(|h| h.cur_scope().block.clone()),
                };
                return run_method(&def, self_obj, &args, block, Some(method), Some(owner));
            }
            // No class method above: fall through to the shared error/no-op paths
            // below with the class name as the linearization root.
            let _ = cls;
        }
    }
    // Linearize from the receiver's actual class so prepend/include super hits
    // the next method in ancestry order; class-method super (self_obj is a class
    // ref, no object class) falls back to the owner's chain.
    let recv_class = with_host(|h| h.object_class(&self_obj)).unwrap_or_else(|| def_class.clone());
    let Some((def, owner)) = with_host(|h| h.find_super(&recv_class, &def_class, &method)) else {
        // No user-defined super method in the ancestor chain. A `super` from a
        // user `initialize` up to a native superclass initializer is the common
        // case: `Exception#initialize(msg)` records the message; every other
        // `Object#initialize` is a no-op. Anything else is a genuine error.
        if method == "initialize" {
            let args = explicit_args.unwrap_or(cur_args);
            let is_exc = with_host(|h| h.is_exception_class(&recv_class));
            if is_exc {
                if let Some(msg) = args.first() {
                    with_host(|h| h.set_ivar_of(&self_obj, "message", msg.clone()));
                }
            }
            return Ok(Value::Undef);
        }
        // `super` from an override of a base Object hook whose default is native,
        // not a Ruby method. `Object#respond_to_missing?` defaults to false, so
        // `name.start_with?("x") || super` in an override resolves cleanly.
        if method == "respond_to_missing?" {
            return Ok(Value::Bool(false));
        }
        // `super` from an override of a Module/Class lifecycle hook whose default
        // is a native no-op. activesupport concerns call `super` in `included`/
        // `extended` to chain up the ancestry; the base hook returns nil.
        if matches!(
            method.as_str(),
            "included"
                | "extended"
                | "prepended"
                | "inherited"
                | "method_added"
                | "method_removed"
                | "method_undefined"
                | "singleton_method_added"
                | "singleton_method_removed"
                | "singleton_method_undefined"
        ) {
            return Ok(Value::Undef);
        }
        // `super` from an override of the native `Module#autoload`. activesupport's
        // `Autoload#autoload` derives a path from the constant name by convention,
        // then calls `super(const, path)` to register it. Register natively under
        // the receiver's namespace.
        if method == "autoload" {
            let args = explicit_args.unwrap_or(cur_args);
            if let (Some(const_arg), Some(path_arg)) = (args.first(), args.get(1)) {
                if let Some(cls) = with_host(|h| h.classref_name(&self_obj)) {
                    let const_name = with_host(|h| h.to_s(const_arg));
                    let path = with_host(|h| h.as_str(path_arg)).unwrap_or_default();
                    let full = format!("{cls}::{const_name}");
                    with_host(|h| h.set_autoload(&full, &path));
                }
            }
            return Ok(Value::Undef);
        }
        // `super` from a `def self.new` override — the default `Class#new`:
        // allocate an instance of the *receiver* class (so a subclass's `new`
        // makes the subclass) and run its `initialize`.
        if method == "new" {
            let args = explicit_args.unwrap_or(cur_args);
            let target =
                with_host(|h| h.classref_name(&self_obj)).unwrap_or_else(|| recv_class.clone());
            let obj = with_host(|h| h.new_object(&target));
            if let Some((def, owner)) = with_host(|h| h.find_method_owner(&target, "initialize")) {
                run_method(
                    &def,
                    obj.clone(),
                    &args,
                    block_override,
                    Some("initialize".into()),
                    Some(owner),
                )?;
            }
            return Ok(obj);
        }
        return Err(format!("super: no superclass method '{method}'"));
    };
    let args = explicit_args.unwrap_or(cur_args);
    // A `super { … }` block overrides; otherwise forward the current block.
    let block = match block_override {
        Some(b) => Some(b),
        None => with_host(|h| h.cur_scope().block.clone()),
    };
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
                    env.lock().unwrap().vars.insert(p, v);
                }
                None => {
                    env.lock().unwrap().vars.shift_remove(&p);
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
            // A bare `rescue` (no classes, no splat) catches StandardError.
            let is_bare = rd.classes.is_empty() && rd.splat.is_none();
            let static_match = rd
                .classes
                .iter()
                .any(|c| with_host(|h| h.exc_matches(&exc_class, c)));
            // A `rescue *expr` splat: run its proc for the class(es) and match.
            let splat_match = if !is_bare && !static_match {
                match rd.splat {
                    Some(sid) => {
                        let cv = run_template(sid, &[])?;
                        let names: Vec<String> = with_host(|h| match h.as_array(&cv) {
                            Some(arr) => arr.iter().filter_map(|v| h.classref_name(v)).collect(),
                            None => h.classref_name(&cv).into_iter().collect(),
                        });
                        names
                            .iter()
                            .any(|c| with_host(|h| h.exc_matches(&exc_class, c)))
                    }
                    None => false,
                }
            } else {
                false
            };
            let matches = is_bare || static_match || splat_match;
            if matches {
                let args = if rd.binding.is_some() {
                    vec![excv.clone()]
                } else {
                    vec![]
                };
                // Ruby exposes the exception being handled as `$!` for the
                // duration of the clause (whether or not it is bound via
                // `=> e`), then restores the prior value on exit — supporting
                // nested begin/rescue.
                let prev_bang = with_host(|h| h.get_global("!"));
                with_host(|h| h.set_global("!", excv.clone()));
                result = run_template(rd.body, &args);
                with_host(|h| h.set_global("!", prev_bang));
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

// ---- Fiber (stackful coroutines, same-thread via corosensei) ----------------

thread_local! {
    /// Id of the fiber whose body is currently executing on this thread, or
    /// `None` at the root. `Fiber.yield` suspends this fiber; yielding at the
    /// root is a FiberError.
    static CUR_FIBER: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };

    /// Live fibers for THIS thread, indexed by `RObj::Fiber.id`. Fibers are
    /// thread-owned — a corosensei `Coroutine` holds a native stack plus a raw
    /// yielder pointer valid only on its creating thread — so they live here, not
    /// on the shared `Send` `RubyHost`. (MRI likewise forbids resuming a fiber on
    /// a thread other than the one that created it.)
    static FIBERS: RefCell<Vec<FiberCell>> = const { RefCell::new(Vec::new()) };

    /// This OS thread's own `Thread.current` object, created on first request.
    static CURRENT_THREAD: RefCell<Option<Value>> = const { RefCell::new(None) };

    /// A stable "main fiber" object returned by `Fiber.current` at the root
    /// (outside any `Fiber.new` body), created on first request.
    static MAIN_FIBER: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// `Fiber.current` — a stable object identifying the running fiber. At the root
/// this is a cached main-fiber object; only identity is modeled (i18n stores it
/// as a config `owner` and later compares), not resume/alive state. Full
/// per-fiber identity inside a `Fiber.new` body is not distinguished yet.
pub fn current_fiber() -> Value {
    if let Some(v) = MAIN_FIBER.with(|c| c.borrow().clone()) {
        return v;
    }
    let v = with_host(|h| h.new_object("Fiber"));
    MAIN_FIBER.with(|c| *c.borrow_mut() = Some(v.clone()));
    v
}

/// `Thread.current` — a stable `Thread` object for the running OS thread, cached
/// per-thread. It is handle-less (already running, nothing to join); `alive?` is
/// true and `join`/`value` are no-ops that return it/nil.
pub fn current_thread() -> Value {
    if let Some(v) = CURRENT_THREAD.with(|c| c.borrow().clone()) {
        return v;
    }
    let v = with_host(|h| {
        let id = h.threads.len() as u32;
        h.threads.push(ThreadCell {
            handle: None,
            result: Arc::new(Mutex::new(None)),
            exc: Arc::new(Mutex::new(None)),
            done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        h.alloc(RObj::Thread { id })
    });
    CURRENT_THREAD.with(|c| *c.borrow_mut() = Some(v.clone()));
    v
}

/// Access this thread's fiber table.
fn with_fibers<R>(f: impl FnOnce(&mut Vec<FiberCell>) -> R) -> R {
    FIBERS.with(|c| f(&mut c.borrow_mut()))
}

impl RubyHost {
    /// Swap the volatile execution context in one shot, returning the previous
    /// one. Used to install a fiber's context on resume and pull it back out on
    /// suspend/return, keeping caller and fiber execution states isolated.
    fn install_fiber_ctx(&mut self, mut c: FiberContext) -> FiberContext {
        std::mem::swap(&mut self.active_scope, &mut c.active_scope);
        std::mem::swap(&mut self.signal, &mut c.signal);
        std::mem::swap(&mut self.pending_exc, &mut c.pending_exc);
        std::mem::swap(&mut self.error, &mut c.error);
        std::mem::swap(&mut self.frames, &mut c.frames);
        std::mem::swap(&mut self.enum_sinks, &mut c.enum_sinks);
        std::mem::swap(&mut self.around_stack, &mut c.around_stack);
        c
    }
}

/// The root frame a fiber's execution context starts with, so `cur_scope` never
/// hits an empty `frames` before the fiber body's proc pushes its own frame.
fn fiber_root_frame() -> Frame {
    Frame {
        scope: Scope {
            locals: new_env(),
            block: None,
            self_obj: Value::Undef,
            method_name: None,
            def_class: None,
        },
        args: Vec::new(),
        line: 0,
    }
}

/// `Fiber.new { |first| ... }`: build a suspended stackful coroutine whose body
/// runs the block. Nothing executes until the first `resume`.
pub fn new_fiber(block: Value) -> Value {
    let id = with_fibers(|fibers| {
        let id = fibers.len() as u32;
        fibers.push(FiberCell {
            coro: None,
            yielder: std::ptr::null(),
            ctx: FiberContext {
                frames: vec![fiber_root_frame()],
                ..FiberContext::default()
            },
            done: false,
        });
        id
    });
    let coro = corosensei::Coroutine::new(
        move |yielder: &corosensei::Yielder<Value, Value>, first: Value| {
            // Same thread → publish the yielder pointer so `Fiber.yield` (running
            // deep inside this body's VM) can reach it. Valid for the body's life.
            with_fibers(|fibers| fibers[id as usize].yielder = yielder as *const _ as *const ());
            // The first resume value becomes the block's single parameter (MRI).
            call_proc(&block, std::slice::from_ref(&first))
        },
    );
    with_fibers(|fibers| fibers[id as usize].coro = Some(coro));
    with_host(|h| h.alloc(RObj::Fiber { id }))
}

/// `Fiber.yield(v)` — suspend the running fiber, handing `v` to `resume`'s
/// caller; returns the value the next `resume(x)` supplies. FiberError at root.
pub fn fiber_yield(v: Value) -> Result<Value, String> {
    let id = match CUR_FIBER.with(|c| c.get()) {
        Some(id) => id,
        None => {
            return Err(crate::builtins::raise_exc(
                "FiberError",
                "can't yield from root fiber",
            ))
        }
    };
    let yp = with_fibers(|fibers| fibers[id as usize].yielder);
    // SAFETY: same-thread coroutine; the yielder lives for the whole fiber body,
    // and we only reach here from inside that body (its stack is live).
    let yielder = unsafe { &*(yp as *const corosensei::Yielder<Value, Value>) };
    Ok(yielder.suspend(v))
}

/// `fiber.resume(v)` — run the fiber until its next `Fiber.yield` or its block
/// returns. FiberError on a dead (returned) fiber. Preserves the shared host:
/// the coroutine is taken out so the body re-enters `with_host` freely, and the
/// volatile context is swapped so the caller's scope/signal survive the switch.
pub fn fiber_resume(fiber: &Value, v: Value) -> Result<Value, String> {
    let id = match with_host(|h| h.obj(fiber).cloned()) {
        Some(RObj::Fiber { id }) => id,
        _ => return Err("not a fiber".into()),
    };
    if with_fibers(|fibers| fibers[id as usize].done) {
        return Err(crate::builtins::raise_exc(
            "FiberError",
            "dead fiber called",
        ));
    }
    let mut coro = with_fibers(|fibers| fibers[id as usize].coro.take())
        .ok_or_else(|| crate::builtins::raise_exc("FiberError", "double resume of a fiber"))?;

    // Install the fiber's context; keep the caller's in a local across resume.
    let fiber_ctx = with_fibers(|fibers| std::mem::take(&mut fibers[id as usize].ctx));
    let caller_ctx = with_host(|h| h.install_fiber_ctx(fiber_ctx));
    let prev = CUR_FIBER.with(|c| c.replace(Some(id)));

    let out = coro.resume(v); // no host borrow held; body drives its own VM

    CUR_FIBER.with(|c| c.set(prev));
    // Pull the fiber's context back out, restore the caller's.
    let fiber_ctx = with_host(|h| h.install_fiber_ctx(caller_ctx));
    with_fibers(|fibers| {
        fibers[id as usize].ctx = fiber_ctx;
        fibers[id as usize].coro = Some(coro);
    });

    match out {
        corosensei::CoroutineResult::Yield(y) => Ok(y),
        corosensei::CoroutineResult::Return(r) => {
            with_fibers(|fibers| fibers[id as usize].done = true);
            r // block's value, or a propagated raise
        }
    }
}

/// `fiber.alive?` — false once the block has returned.
pub fn fiber_alive(fiber: &Value) -> bool {
    match with_host(|h| h.obj(fiber).cloned()) {
        Some(RObj::Fiber { id }) => with_fibers(|fibers| !fibers[id as usize].done),
        _ => false,
    }
}

// ---- Thread (real OS threads serialized by the GVL) ------------------------

/// `Thread.new { ... }` — spawn an OS thread running `block` under the GVL.
/// The spawner holds the GVL, so the new thread blocks on `gvl_enter` until the
/// spawner releases it (at `join`/`value`/`sleep`), giving MRI's one-Ruby-thread-
/// at-a-time semantics. Returns a `Thread` object.
pub fn spawn_thread(block: Value) -> Value {
    use std::sync::atomic::AtomicBool;
    let result: Arc<Mutex<Option<Result<Value, String>>>> = Arc::new(Mutex::new(None));
    let exc: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let done = Arc::new(AtomicBool::new(false));
    let (r2, e2, d2) = (result.clone(), exc.clone(), done.clone());
    // Capture the spawner's VM so the child shares this program's heap (not a
    // fresh one). Cloning the `Arc` here is safe even though we hold the GVL:
    // it only bumps the refcount, it does not lock or swap the current VM.
    let parent_vm = current_vm();
    let handle = std::thread::spawn(move || {
        // Bind this OS thread to the parent's VM before running any Ruby, so its
        // `gvl_enter` locks the shared host (and blocks until the spawner yields
        // the GVL) instead of creating an isolated one.
        install_current_vm(parent_vm);
        let (out, raised) = run_thread_body(&block);
        *r2.lock().unwrap() = Some(out);
        *e2.lock().unwrap() = raised;
        d2.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    with_host(|h| {
        let id = h.threads.len() as u32;
        h.threads.push(ThreadCell {
            handle: Some(handle),
            result,
            exc,
            done,
        });
        h.alloc(RObj::Thread { id })
    })
}

/// The body an OS thread runs: acquire the GVL, install a fresh execution context
/// (so this thread's frames/scope/signal don't clobber the spawner's, which live
/// in the shared host), run the block, then restore the spawner's context. On a
/// raise, the exception object is captured before the context is torn down.
fn run_thread_body(block: &Value) -> (Result<Value, String>, Option<Value>) {
    with_gvl(|| {
        let fresh = FiberContext {
            frames: vec![fiber_root_frame()],
            ..FiberContext::default()
        };
        let saved = with_host(|h| h.install_fiber_ctx(fresh));
        let r = call_proc(block, &[]);
        let raised = if r.is_err() {
            with_host(|h| h.take_pending_exc())
        } else {
            None
        };
        with_host(|h| h.install_fiber_ctx(saved));
        (r, raised)
    })
}

/// `Thread#join`/`#value` — release the GVL, wait for the OS thread to finish,
/// reacquire, and return its stored outcome (an `Err` is the raised exception,
/// re-raised by `value` / by `join` when it propagates). Idempotent: only the
/// first call owns the `JoinHandle`; later calls just read the result.
pub fn thread_join(thread: &Value) -> Result<Value, String> {
    let id = match with_host(|h| h.obj(thread).cloned()) {
        Some(RObj::Thread { id }) => id as usize,
        _ => return Err("not a thread".into()),
    };
    let handle = with_host(|h| h.threads.get_mut(id).and_then(|t| t.handle.take()));
    if let Some(handle) = handle {
        // Drop the GVL so the spawned thread can actually run, then wait for it.
        gvl_blocking(move || {
            let _ = handle.join();
        });
    }
    let (result, raised) = with_host(|h| {
        h.threads
            .get(id)
            .map(|t| {
                (
                    t.result.lock().unwrap().clone(),
                    t.exc.lock().unwrap().clone(),
                )
            })
            .unwrap_or((None, None))
    });
    // Re-raise the real exception object so a `rescue => e` binds it (with
    // `#message`/`#class`), matching MRI's `Thread#value`.
    if let Some(exc) = raised {
        with_host(|h| h.set_pending_exc(exc));
    }
    result.unwrap_or(Ok(Value::Undef))
}

/// `Thread#alive?` — true until the body has finished.
pub fn thread_alive(thread: &Value) -> bool {
    match with_host(|h| h.obj(thread).cloned()) {
        Some(RObj::Thread { id }) => with_host(|h| {
            h.threads
                .get(id as usize)
                .map(|t| !t.done.load(std::sync::atomic::Ordering::SeqCst))
                .unwrap_or(false)
        }),
        _ => false,
    }
}

// ---- Queue / SizedQueue (thread-safe, blocking) ----------------------------

/// Register a new queue (`cap = Some(n)` → `SizedQueue`). Returns its id, stored
/// in the object's `__qid` ivar.
pub fn new_queue(cap: Option<usize>) -> u32 {
    with_host(|h| {
        let id = h.queues.len() as u32;
        h.queues.push(Arc::new(QueueSync {
            data: Mutex::new(QueueData {
                items: std::collections::VecDeque::new(),
                closed: false,
                cap,
            }),
            cv: std::sync::Condvar::new(),
        }));
        id
    })
}

/// Clone the queue's shared sync handle out of the host so blocking waits happen
/// without holding the GVL.
fn queue_sync(id: u32) -> Option<Arc<QueueSync>> {
    with_host(|h| h.queues.get(id as usize).cloned())
}

/// `Queue#push(v)` — append and wake a waiter. A `SizedQueue` blocks (GVL
/// released) while full, unless `non_block` (then raises `ThreadError`).
pub fn queue_push(id: u32, v: Value, non_block: bool) -> Result<Value, String> {
    let q = match queue_sync(id) {
        Some(q) => q,
        None => return Ok(Value::Undef),
    };
    let full = {
        let d = q.data.lock().unwrap();
        d.cap.is_some_and(|c| d.items.len() >= c)
    };
    if full {
        if non_block {
            return Err(crate::builtins::raise_exc("ThreadError", "queue full"));
        }
        gvl_blocking(|| {
            let mut d = q.data.lock().unwrap();
            while d.cap.is_some_and(|c| d.items.len() >= c) && !d.closed {
                d = q.cv.wait(d).unwrap();
            }
        });
    }
    {
        let mut d = q.data.lock().unwrap();
        if d.closed {
            return Err(crate::builtins::raise_exc(
                "ClosedQueueError",
                "queue closed",
            ));
        }
        d.items.push_back(v);
    }
    q.cv.notify_all();
    Ok(Value::Undef)
}

/// `Queue#pop` — remove the front; if empty, block (GVL released) until a `push`,
/// unless `non_block` (raise `ThreadError`) or the queue is closed and drained
/// (return `nil`).
pub fn queue_pop(id: u32, non_block: bool) -> Result<Value, String> {
    let q = match queue_sync(id) {
        Some(q) => q,
        None => return Ok(Value::Undef),
    };
    loop {
        {
            let mut d = q.data.lock().unwrap();
            if let Some(v) = d.items.pop_front() {
                drop(d);
                q.cv.notify_all(); // wake a SizedQueue push blocked on a full queue
                return Ok(v);
            }
            if d.closed {
                return Ok(Value::Undef);
            }
        }
        if non_block {
            return Err(crate::builtins::raise_exc("ThreadError", "queue empty"));
        }
        // Empty: release the GVL so a producer can run, and park until a push.
        gvl_blocking(|| {
            let mut d = q.data.lock().unwrap();
            while d.items.is_empty() && !d.closed {
                d = q.cv.wait(d).unwrap();
            }
        });
    }
}

/// `Queue#length`/`#size`.
pub fn queue_len(id: u32) -> usize {
    queue_sync(id).map_or(0, |q| q.data.lock().unwrap().items.len())
}

/// `Queue#close` — no more pushes; blocked pops drain then return `nil`.
pub fn queue_close(id: u32) {
    if let Some(q) = queue_sync(id) {
        q.data.lock().unwrap().closed = true;
        q.cv.notify_all();
    }
}

/// `Queue#closed?`.
pub fn queue_closed(id: u32) -> bool {
    queue_sync(id).is_some_and(|q| q.data.lock().unwrap().closed)
}

/// `Queue#clear`.
pub fn queue_clear(id: u32) {
    if let Some(q) = queue_sync(id) {
        q.data.lock().unwrap().items.clear();
    }
}

// ---- ConditionVariable -----------------------------------------------------

/// Register a new `ConditionVariable`; returns its id (the `__cvid` ivar).
pub fn new_condvar() -> u32 {
    with_host(|h| {
        let id = h.condvars.len() as u32;
        h.condvars.push(Arc::new(CondVarSync {
            gen: Mutex::new(0),
            cv: std::sync::Condvar::new(),
        }));
        id
    })
}

fn condvar_sync(id: u32) -> Option<Arc<CondVarSync>> {
    with_host(|h| h.condvars.get(id as usize).cloned())
}

/// `ConditionVariable#wait(mutex)` — release the GVL and park until `signal`/
/// `broadcast` bumps the generation past the value captured while holding the
/// mutex (so a signal delivered after we start waiting is never missed). The
/// caller unlocks the Ruby `mutex` before and relocks it after.
pub fn condvar_wait(id: u32) {
    let Some(c) = condvar_sync(id) else { return };
    gvl_blocking(|| {
        let mut g = c.gen.lock().unwrap();
        let start = *g;
        while *g == start {
            g = c.cv.wait(g).unwrap();
        }
    });
}

/// `ConditionVariable#signal` (`all = false`) / `#broadcast` (`all = true`).
pub fn condvar_notify(id: u32, all: bool) {
    if let Some(c) = condvar_sync(id) {
        *c.gen.lock().unwrap() += 1;
        if all {
            c.cv.notify_all();
        } else {
            c.cv.notify_one();
        }
    }
}

// ---- SQLite3 side table ---------------------------------------------------

/// Open a `SQLite3::Database` on `path` (`":memory:"` — or the empty string — for
/// an in-memory DB), registering the owned `rusqlite::Connection` in the host
/// side table and returning a fresh `RObj::Db` handle. The connection is opened
/// before the host borrow so no nested `with_host` is held. Errors (bad path,
/// permissions) surface as the message for a `SQLite3::SQLException`.
pub fn db_open(path: &str) -> Result<Value, String> {
    let conn = if path == ":memory:" || path.is_empty() {
        rusqlite::Connection::open_in_memory()
    } else {
        rusqlite::Connection::open(path)
    }
    .map_err(|e| e.to_string())?;
    Ok(with_host(|h| {
        let id = h.db_handles.len() as u32;
        h.db_handles.push(Some(DbCell {
            conn,
            results_as_hash: false,
        }));
        h.alloc(RObj::Db { id })
    }))
}

/// The `db_handles` index behind a `Db` value, if `v` is one.
fn db_id(v: &Value) -> Option<u32> {
    match with_host(|h| h.obj(v).cloned()) {
        Some(RObj::Db { id }) => Some(id),
        _ => None,
    }
}

/// Prepare `sql`, bind the positional `binds`, run it, and collect the result as
/// `(column_names, rows)` where each row is the raw sqlite column values. The
/// whole prepare/step loop runs inside a single `with_host` borrow because a
/// `SqlVal` (`rusqlite::types::Value`) is owned native data that never touches
/// the object heap — the caller converts to Ruby `Value`s afterward, avoiding a
/// second `&mut self` borrow while the connection is live. Non-SELECT statements
/// (DDL/DML) execute here too and simply return zero rows.
pub fn db_execute(
    v: &Value,
    sql: &str,
    binds: &[SqlVal],
) -> Result<(Vec<String>, Vec<Vec<SqlVal>>), String> {
    let id = db_id(v).ok_or_else(|| "not a database handle".to_string())?;
    with_host(|h| {
        let cell = h
            .db_handles
            .get(id as usize)
            .and_then(|c| c.as_ref())
            .ok_or_else(|| "cannot use a closed database".to_string())?;
        let mut stmt = cell.conn.prepare(sql).map_err(|e| e.to_string())?;
        let ncol = stmt.column_count();
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        // The sqlite3 gem leaves any placeholder with no supplied bind as NULL
        // (`execute(sql, "one")` against two `?`s binds the second to NULL). Pad
        // to the statement's parameter count so rusqlite's strict count check
        // matches that lenient behavior.
        let nparams = stmt.parameter_count();
        let mut binds = binds.to_vec();
        binds.resize(binds.len().max(nparams), SqlVal::Null);
        let mut rows = stmt
            .query(rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let mut r = Vec::with_capacity(ncol);
            for i in 0..ncol {
                let val: SqlVal = row.get(i).map_err(|e| e.to_string())?;
                r.push(val);
            }
            out.push(r);
        }
        Ok((cols, out))
    })
}

/// `db.last_insert_row_id` — the rowid of the most recent successful INSERT.
pub fn db_last_insert_rowid(v: &Value) -> i64 {
    match db_id(v) {
        Some(id) => with_host(|h| {
            h.db_handles
                .get(id as usize)
                .and_then(|c| c.as_ref())
                .map(|c| c.conn.last_insert_rowid())
                .unwrap_or(0)
        }),
        None => 0,
    }
}

/// `db.changes` — rows modified by the most recent INSERT/UPDATE/DELETE.
pub fn db_changes(v: &Value) -> i64 {
    match db_id(v) {
        Some(id) => with_host(|h| {
            h.db_handles
                .get(id as usize)
                .and_then(|c| c.as_ref())
                .map(|c| c.conn.changes() as i64)
                .unwrap_or(0)
        }),
        None => 0,
    }
}

/// `db.close` — drop the connection (closing the file), leaving the handle
/// closed. Idempotent.
pub fn db_close(v: &Value) {
    if let Some(id) = db_id(v) {
        with_host(|h| {
            if let Some(slot) = h.db_handles.get_mut(id as usize) {
                *slot = None;
            }
        });
    }
}

/// `db.closed?` — true once `close` has run (or `v` is not a live handle).
pub fn db_closed(v: &Value) -> bool {
    match db_id(v) {
        Some(id) => with_host(|h| !matches!(h.db_handles.get(id as usize), Some(Some(_)))),
        None => true,
    }
}

/// `db.results_as_hash = flag`.
pub fn db_set_results_as_hash(v: &Value, on: bool) {
    if let Some(id) = db_id(v) {
        with_host(|h| {
            if let Some(Some(cell)) = h.db_handles.get_mut(id as usize) {
                cell.results_as_hash = on;
            }
        });
    }
}

/// Whether `db.results_as_hash` is set (rows returned as Hashes).
pub fn db_results_as_hash(v: &Value) -> bool {
    match db_id(v) {
        Some(id) => with_host(
            |h| matches!(h.db_handles.get(id as usize), Some(Some(c)) if c.results_as_hash),
        ),
        None => false,
    }
}

// ---- Fiddle (FFI) side table ----------------------------------------------

/// `Fiddle.dlopen(path)` / `Fiddle::Handle.new(path)`. A `nil`/empty path opens
/// the current process' global symbol scope (`dlopen(NULL)`), so libc symbols
/// already loaded into the process — `strlen`, `abs`, `sqrt`, `getenv` — are
/// resolvable without naming a library file. A path opens that shared object
/// with `RTLD_LAZY | RTLD_GLOBAL` (the sqlite3-gem/MRI default). Returns a fresh
/// `RObj::FiddleHandle` value.
pub fn fiddle_dlopen(path: Option<&str>) -> Result<Value, String> {
    let lib = match path {
        None => libloading::os::unix::Library::this(),
        Some(p) => {
            // SAFETY: `dlopen` of a user-named library. This runs arbitrary
            // constructor code in the loaded object, exactly as MRI's
            // `Fiddle.dlopen` does — the operation is inherently unsafe and its
            // safety is the caller's responsibility (a bad path only errors).
            unsafe {
                libloading::os::unix::Library::open(Some(p), libc::RTLD_LAZY | libc::RTLD_GLOBAL)
                    .map_err(|e| e.to_string())?
            }
        }
    };
    Ok(with_host(|h| {
        let id = h.fiddle_libs.len() as u32;
        h.fiddle_libs.push(Some(FiddleLib(lib)));
        h.alloc(RObj::FiddleHandle { id })
    }))
}

/// Resolve `name` in a `Fiddle::Handle` to its raw code address (`handle[name]`
/// / `handle.sym(name)`). Errors if the handle is closed or the symbol is
/// missing (MRI raises `Fiddle::DLError`).
pub fn fiddle_sym(v: &Value, name: &str) -> Result<u64, String> {
    let id = match with_host(|h| h.obj(v).cloned()) {
        Some(RObj::FiddleHandle { id }) => id,
        _ => return Err("not a Fiddle::Handle".to_string()),
    };
    with_host(|h| {
        let lib = h
            .fiddle_libs
            .get(id as usize)
            .and_then(|l| l.as_ref())
            .ok_or_else(|| "closed handle".to_string())?;
        // libloading appends a trailing NUL if absent; pass it explicitly.
        let mut sym_bytes = name.as_bytes().to_vec();
        sym_bytes.push(0);
        // SAFETY: `dlsym`. The returned `Symbol` borrows the library; `into_raw`
        // detaches it into a bare address that stays valid while the library is
        // loaded (it lives in `fiddle_libs` until `#close`). We only read the
        // address, never call through this typing.
        let sym: libloading::os::unix::Symbol<*mut std::ffi::c_void> =
            unsafe { lib.0.get(&sym_bytes).map_err(|e| e.to_string())? };
        Ok(sym.into_raw() as u64)
    })
}

/// The `(addr, arg type codes, ret type code)` behind a `Fiddle::Function`.
pub fn fiddle_func_parts(v: &Value) -> Option<(u64, Vec<i32>, i32)> {
    match with_host(|h| h.obj(v).cloned()) {
        Some(RObj::FiddleFunc { addr, args, ret }) => Some((addr, args, ret)),
        _ => None,
    }
}

/// The `(addr, size)` behind a `Fiddle::Pointer`.
pub fn fiddle_ptr_parts(v: &Value) -> Option<(u64, i64)> {
    match with_host(|h| h.obj(v).cloned()) {
        Some(RObj::FiddlePtr { addr, size, .. }) => Some((addr, size)),
        _ => None,
    }
}

/// Build a `Fiddle::Function` value from a resolved address and its runtime
/// signature (argument type codes + return type code).
pub fn fiddle_func_new(addr: u64, args: Vec<i32>, ret: i32) -> Value {
    with_host(|h| h.alloc(RObj::FiddleFunc { addr, args, ret }))
}

/// `handle.close` — drop the library (unloads it), leaving the handle closed.
pub fn fiddle_handle_close(v: &Value) {
    if let Some(RObj::FiddleHandle { id }) = with_host(|h| h.obj(v).cloned()) {
        with_host(|h| {
            if let Some(slot) = h.fiddle_libs.get_mut(id as usize) {
                *slot = None;
            }
        });
    }
}

/// Allocate an owned, heap-backed `Fiddle::Pointer` from `buf` and record its
/// stable data address. `size` is the logical byte size exposed to Ruby
/// (`Pointer#size`); `buf` may carry a trailing NUL beyond `size` so `#to_s`
/// reads a valid C string.
pub fn fiddle_alloc(buf: Vec<u8>, size: i64) -> Value {
    with_host(|h| {
        let id = h.fiddle_mem.len() as u32;
        h.fiddle_mem.push(Some(buf.into_boxed_slice()));
        let addr = h.fiddle_mem[id as usize].as_ref().unwrap().as_ptr() as u64;
        h.alloc(RObj::FiddlePtr {
            addr,
            size,
            owned: Some(id),
        })
    })
}

/// A `Fiddle::Pointer` that borrows memory it does not own (a `TYPE_VOIDP`
/// result, or `Pointer.new(addr)`). `size` 0 means "unknown length".
pub fn fiddle_ptr_raw(addr: u64, size: i64) -> Value {
    with_host(|h| {
        h.alloc(RObj::FiddlePtr {
            addr,
            size,
            owned: None,
        })
    })
}

/// `ptr.free` — release an owned buffer. A no-op on a borrowed pointer.
pub fn fiddle_free(v: &Value) {
    if let Some(RObj::FiddlePtr {
        owned: Some(id), ..
    }) = with_host(|h| h.obj(v).cloned())
    {
        with_host(|h| {
            if let Some(slot) = h.fiddle_mem.get_mut(id as usize) {
                *slot = None;
            }
        });
    }
}

/// Read a NUL-terminated C string at `addr`. Empty for a null pointer.
///
/// SAFETY: dereferences a raw address the caller asserts is a valid, live,
/// NUL-terminated C string. A wrong address crashes the process — this is the
/// documented low-level contract of `Fiddle::Pointer#to_s`, matching MRI.
pub fn fiddle_read_cstr(addr: u64) -> String {
    if addr == 0 {
        return String::new();
    }
    unsafe {
        std::ffi::CStr::from_ptr(addr as *const std::os::raw::c_char)
            .to_string_lossy()
            .into_owned()
    }
}

/// Read exactly `len` bytes at `addr` as a (lossily-decoded) String. Empty for a
/// null pointer or zero length.
///
/// SAFETY: reads `len` bytes from a raw address the caller asserts is valid and
/// at least `len` bytes long (Fiddle's low-level contract).
pub fn fiddle_read_bytes(addr: u64, len: usize) -> String {
    if addr == 0 || len == 0 {
        return String::new();
    }
    unsafe {
        let sl = std::slice::from_raw_parts(addr as *const u8, len);
        String::from_utf8_lossy(sl).into_owned()
    }
}

/// Read one raw byte at `addr` (`Fiddle::Pointer#[i]`), unmangled — the `String`
/// read path lossily re-encodes non-UTF-8 bytes, so a direct byte read is needed.
pub fn fiddle_read_byte(addr: u64) -> u8 {
    if addr == 0 {
        return 0;
    }
    unsafe { *(addr as *const u8) }
}

/// Write `bytes` into the memory at `addr` (`Fiddle::Pointer#[]=`). The caller
/// clamps the length to the pointer's own buffer size, so this never writes past
/// an owned `malloc` allocation. A null address or empty slice is a no-op.
pub fn fiddle_write_bytes(addr: u64, bytes: &[u8]) {
    if addr == 0 || bytes.is_empty() {
        return;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
    }
}

/// The default `#to_s` reading used by host `to_s`: a known-size pointer reads
/// that many bytes, else it reads up to the first NUL. Used so `puts ptr` /
/// interpolation match `Fiddle::Pointer#to_s`.
pub fn fiddle_read_cstr_or_len(addr: u64, size: i64) -> String {
    if size > 0 {
        // A sized buffer that ends in (or contains) a NUL still stops at it,
        // matching MRI, so `Pointer["abc"].to_s` is "abc" not "abc\0…".
        let raw = fiddle_read_bytes(addr, size as usize);
        match raw.find('\0') {
            Some(i) => raw[..i].to_string(),
            None => raw,
        }
    } else {
        fiddle_read_cstr(addr)
    }
}

// ---- IO / File side table -------------------------------------------------

/// Register an owned `std::fs::File` (opened by `File.open`/`File.read`/…) in the
/// host side table and return a fresh `IoHandle` value pointing at it.
pub fn io_alloc_file(file: std::fs::File, path: String) -> Value {
    with_host(|h| {
        let id = h.io_handles.len() as u32;
        h.io_handles.push(IoCell::File {
            file: Some(file),
            path,
        });
        h.alloc(RObj::IoHandle { id })
    })
}

/// The `io_handles` index behind an `IoHandle` value, if `v` is one.
fn io_id(v: &Value) -> Option<u32> {
    match with_host(|h| h.obj(v).cloned()) {
        Some(RObj::IoHandle { id }) => Some(id),
        _ => None,
    }
}

/// Whether this handle is closed (`File#closed?`). Standard streams never close.
pub fn io_closed(v: &Value) -> bool {
    match io_id(v) {
        Some(id) => with_host(|h| {
            matches!(
                h.io_handles.get(id as usize),
                Some(IoCell::File { file: None, .. })
            )
        }),
        None => false,
    }
}

/// `IO#write` for one already-stringified chunk; returns the byte count written.
pub fn io_write_str(v: &Value, s: &str) -> Result<usize, String> {
    use std::io::Write;
    let id = io_id(v).ok_or("not an IO")?;
    with_host(|h| match h.io_handles.get_mut(id as usize) {
        Some(IoCell::Stdout) => {
            let mut o = std::io::stdout();
            o.write_all(s.as_bytes())
                .and_then(|_| o.flush())
                .map(|_| s.len())
                .map_err(|e| e.to_string())
        }
        Some(IoCell::Stderr) => {
            let mut o = std::io::stderr();
            o.write_all(s.as_bytes())
                .and_then(|_| o.flush())
                .map(|_| s.len())
                .map_err(|e| e.to_string())
        }
        Some(IoCell::Stdin) => Err("not opened for writing".to_string()),
        Some(IoCell::File { file: Some(f), .. }) => f
            .write_all(s.as_bytes())
            .map(|_| s.len())
            .map_err(|e| e.to_string()),
        Some(IoCell::File { file: None, .. }) => Err("closed stream".to_string()),
        Some(IoCell::TcpListener { .. }) | Some(IoCell::TcpStream { .. }) => {
            Err("not an IO".to_string())
        }
        None => Err("not an IO".to_string()),
    })
}

/// `IO#read` (no length): read everything remaining from the current position.
pub fn io_read_all(v: &Value) -> Result<String, String> {
    use std::io::Read;
    let id = io_id(v).ok_or("not an IO")?;
    with_host(|h| {
        let mut s = String::new();
        match h.io_handles.get_mut(id as usize) {
            Some(IoCell::File { file: Some(f), .. }) => {
                f.read_to_string(&mut s).map_err(|e| e.to_string())?;
                Ok(s)
            }
            Some(IoCell::File { file: None, .. }) => Err("closed stream".to_string()),
            Some(IoCell::Stdin) => {
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| e.to_string())?;
                Ok(s)
            }
            Some(IoCell::Stdout) | Some(IoCell::Stderr) => {
                Err("not opened for reading".to_string())
            }
            Some(IoCell::TcpListener { .. }) | Some(IoCell::TcpStream { .. }) => {
                Err("not an IO".to_string())
            }
            None => Err("not an IO".to_string()),
        }
    })
}

/// `IO#gets`: read one line up to and including the next `\n` (or EOF). Returns
/// nil at EOF. Byte-oriented so the file cursor advances line by line.
pub fn io_gets(v: &Value) -> Result<Value, String> {
    use std::io::Read;
    let id = io_id(v).ok_or("not an IO")?;
    with_host(|h| {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let mut one = [0u8; 1];
            let n = match h.io_handles.get_mut(id as usize) {
                Some(IoCell::File { file: Some(f), .. }) => f.read(&mut one),
                Some(IoCell::File { file: None, .. }) => return Err("closed stream".to_string()),
                Some(IoCell::Stdin) => std::io::stdin().read(&mut one),
                Some(IoCell::Stdout) | Some(IoCell::Stderr) => {
                    return Err("not opened for reading".to_string())
                }
                Some(IoCell::TcpListener { .. }) | Some(IoCell::TcpStream { .. }) => {
                    return Err("not an IO".to_string())
                }
                None => return Err("not an IO".to_string()),
            };
            match n {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(one[0]);
                    if one[0] == b'\n' {
                        break;
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        if buf.is_empty() {
            Ok(Value::Undef)
        } else {
            Ok(h.new_string(String::from_utf8_lossy(&buf).into_owned()))
        }
    })
}

/// `IO#readlines`/`#each_line`: the remaining lines, each keeping its `\n`.
pub fn io_readlines(v: &Value) -> Result<Vec<Value>, String> {
    let all = io_read_all(v)?;
    Ok(with_host(|h| {
        all.split_inclusive('\n')
            .map(|l| h.new_string(l.to_string()))
            .collect()
    }))
}

/// `IO#close`: drop the underlying file (idempotent). A no-op for the standard
/// streams (MRI lets you close them, but we keep the process stdio intact).
pub fn io_close(v: &Value) -> Result<(), String> {
    let id = io_id(v).ok_or("not an IO")?;
    with_host(|h| {
        if let Some(IoCell::File { file, .. }) = h.io_handles.get_mut(id as usize) {
            *file = None;
        }
    });
    Ok(())
}

/// `IO#flush`: flush the underlying stream. Returns unit; the caller returns the
/// IO object (MRI's `flush` returns `self`).
pub fn io_flush(v: &Value) -> Result<(), String> {
    use std::io::Write;
    let id = io_id(v).ok_or("not an IO")?;
    with_host(|h| match h.io_handles.get_mut(id as usize) {
        Some(IoCell::Stdout) => std::io::stdout().flush().map_err(|e| e.to_string()),
        Some(IoCell::Stderr) => std::io::stderr().flush().map_err(|e| e.to_string()),
        Some(IoCell::File { file: Some(f), .. }) => f.flush().map_err(|e| e.to_string()),
        _ => Ok(()),
    })
}

// ---- TCP sockets (std::net) ----------------------------------------------
//
// `TCPServer`/`TCPSocket` reuse the `IoHandle`/`io_handles` side table: a socket
// value is an `RObj::IoHandle` pointing at an `IoCell::TcpListener`/`TcpStream`.
// Every blocking syscall (`accept`, `read`, `write`) is issued on a `try_clone`d
// handle *after* the host `RefCell` borrow is released, so a blocked socket op
// never holds the host lock (a client on another thread has its own thread-local
// host, but never blocking under the borrow keeps the single-thread invariant).

/// Register an owned `IoCell` in the host side table, returning a fresh
/// `IoHandle` value pointing at it.
fn io_alloc_cell(cell: IoCell) -> Value {
    with_host(|h| {
        let id = h.io_handles.len() as u32;
        h.io_handles.push(cell);
        h.alloc(RObj::IoHandle { id })
    })
}

/// A `try_clone`d `TcpStream` for the handle `id` (so the caller can block on a
/// read/write without holding the host borrow). Errors if closed or not a stream.
fn tcp_stream_clone(id: u32) -> Result<std::net::TcpStream, String> {
    with_host(|h| match h.io_handles.get(id as usize) {
        Some(IoCell::TcpStream {
            stream: Some(s), ..
        }) => s.try_clone().map_err(|e| e.to_string()),
        Some(IoCell::TcpStream { stream: None, .. }) => Err("closed stream".to_string()),
        _ => Err("not a TCPSocket".to_string()),
    })
}

/// Bytes currently buffered in the read-ahead buffer of a `TcpStream` handle.
fn tcp_rbuf_len(id: u32) -> Result<usize, String> {
    with_host(|h| match h.io_handles.get(id as usize) {
        Some(IoCell::TcpStream { rbuf, .. }) => Ok(rbuf.len()),
        _ => Err("not a TCPSocket".to_string()),
    })
}

/// Pop up to `max` bytes off the front of the read-ahead buffer.
fn tcp_rbuf_take(id: u32, max: usize) -> Vec<u8> {
    with_host(|h| match h.io_handles.get_mut(id as usize) {
        Some(IoCell::TcpStream { rbuf, .. }) => {
            let n = max.min(rbuf.len());
            rbuf.drain(..n).collect()
        }
        _ => Vec::new(),
    })
}

/// Read one 4 KiB chunk from the socket into its read-ahead buffer (blocking).
/// Returns the number of bytes read (0 = EOF). The blocking `read` runs on a
/// cloned handle with the host borrow released.
fn tcp_fill(id: u32) -> Result<usize, String> {
    use std::io::Read;
    let stream = tcp_stream_clone(id)?;
    let mut buf = [0u8; 4096];
    let n = (&stream).read(&mut buf).map_err(|e| e.to_string())?;
    with_host(|h| {
        if let Some(IoCell::TcpStream { rbuf, .. }) = h.io_handles.get_mut(id as usize) {
            rbuf.extend(&buf[..n]);
        }
    });
    Ok(n)
}

fn tcp_new_string(bytes: &[u8]) -> Value {
    with_host(|h| h.new_string(String::from_utf8_lossy(bytes).into_owned()))
}

/// `TCPServer.new([host,] port)`: bind + listen. `host` defaults to all
/// interfaces; `port` 0 lets the OS assign an ephemeral port (read back with
/// `#addr`).
pub fn tcp_listen(host: &str, port: u16) -> Result<Value, String> {
    let listener = std::net::TcpListener::bind((host, port)).map_err(|e| e.to_string())?;
    let local = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    Ok(io_alloc_cell(IoCell::TcpListener {
        listener: Some(listener),
        local,
    }))
}

/// `TCPServer#accept`: block for the next connection, returning a connected
/// `TCPSocket`. The blocking `accept` runs on a cloned listener with the host
/// borrow released.
pub fn tcp_accept(v: &Value) -> Result<Value, String> {
    let id = io_id(v).ok_or("not a socket")?;
    let listener = with_host(|h| match h.io_handles.get(id as usize) {
        Some(IoCell::TcpListener {
            listener: Some(l), ..
        }) => l.try_clone().map_err(|e| e.to_string()),
        Some(IoCell::TcpListener { listener: None, .. }) => Err("closed stream".to_string()),
        _ => Err("not a TCPServer".to_string()),
    })?;
    let (stream, peer) = listener.accept().map_err(|e| e.to_string())?;
    Ok(io_alloc_cell(IoCell::TcpStream {
        stream: Some(stream),
        peer: peer.to_string(),
        rbuf: std::collections::VecDeque::new(),
    }))
}

/// `TCPSocket.new(host, port)`: connect to a remote endpoint (blocking).
pub fn tcp_connect(host: &str, port: u16) -> Result<Value, String> {
    let stream = std::net::TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    Ok(io_alloc_cell(IoCell::TcpStream {
        stream: Some(stream),
        peer,
        rbuf: std::collections::VecDeque::new(),
    }))
}

/// `TCPSocket#write`/`#<<`/`#print`: write all of `s`, returning the byte count.
pub fn tcp_write(v: &Value, s: &str) -> Result<usize, String> {
    use std::io::Write;
    let id = io_id(v).ok_or("not a socket")?;
    let stream = tcp_stream_clone(id)?;
    (&stream)
        .write_all(s.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(s.len())
}

/// `TCPSocket#gets`: read one line up to and including `\n` (or EOF). Returns nil
/// at EOF with an empty buffer. Buffered via the handle's read-ahead buffer.
pub fn tcp_gets(v: &Value) -> Result<Value, String> {
    let id = io_id(v).ok_or("not a socket")?;
    loop {
        let line = with_host(|h| match h.io_handles.get_mut(id as usize) {
            Some(IoCell::TcpStream { rbuf, .. }) => Ok(rbuf
                .iter()
                .position(|&b| b == b'\n')
                .map(|pos| rbuf.drain(..=pos).collect::<Vec<u8>>())),
            _ => Err("not a TCPSocket".to_string()),
        })?;
        if let Some(bytes) = line {
            return Ok(tcp_new_string(&bytes));
        }
        if tcp_fill(id)? == 0 {
            // EOF: return whatever remains (no trailing newline), else nil.
            let rest = tcp_rbuf_take(id, usize::MAX);
            return Ok(if rest.is_empty() {
                Value::Undef
            } else {
                tcp_new_string(&rest)
            });
        }
    }
}

/// `TCPSocket#read(n)`: read exactly `n` bytes, blocking until `n` are available
/// or EOF. `n == None` reads everything until EOF. Returns nil when `n > 0` and
/// the stream is already at EOF (matching MRI).
pub fn tcp_read(v: &Value, n: Option<usize>) -> Result<Value, String> {
    let id = io_id(v).ok_or("not a socket")?;
    match n {
        Some(n) => {
            while tcp_rbuf_len(id)? < n {
                if tcp_fill(id)? == 0 {
                    break;
                }
            }
            let bytes = tcp_rbuf_take(id, n);
            if bytes.is_empty() && n > 0 {
                return Ok(Value::Undef);
            }
            Ok(tcp_new_string(&bytes))
        }
        None => {
            while tcp_fill(id)? != 0 {}
            let bytes = tcp_rbuf_take(id, usize::MAX);
            Ok(tcp_new_string(&bytes))
        }
    }
}

/// `TCPSocket#readpartial(n)`: return between 1 and `n` bytes, blocking only if
/// the buffer is empty. `Ok(None)` signals EOF (the caller raises `EOFError`).
pub fn tcp_readpartial(v: &Value, n: usize) -> Result<Option<Value>, String> {
    let id = io_id(v).ok_or("not a socket")?;
    if tcp_rbuf_len(id)? == 0 && tcp_fill(id)? == 0 {
        return Ok(None);
    }
    Ok(Some(tcp_new_string(&tcp_rbuf_take(id, n))))
}

/// `TCPSocket#read_nonblock(n)` (best-effort): return immediately with up to `n`
/// buffered/available bytes. `Ok(None)` = EOF; `Err("__EAGAIN__")` = no data
/// ready (the caller raises `IO::EAGAINWaitReadable`). The `O_NONBLOCK` flag is
/// set on the shared open file description for the duration of the one read and
/// then cleared — best-effort, not a full non-blocking IO model.
pub fn tcp_read_nonblock(v: &Value, n: usize) -> Result<Option<Value>, String> {
    use std::io::Read;
    let id = io_id(v).ok_or("not a socket")?;
    if tcp_rbuf_len(id)? > 0 {
        return Ok(Some(tcp_new_string(&tcp_rbuf_take(id, n))));
    }
    let stream = tcp_stream_clone(id)?;
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; n.max(1)];
    let res = (&stream).read(&mut buf);
    let _ = stream.set_nonblocking(false);
    match res {
        Ok(0) => Ok(None),
        Ok(k) => Ok(Some(tcp_new_string(&buf[..k]))),
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Err("__EAGAIN__".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// The `[family, port, host, ip]` address array for a socket, matching MRI's
/// `TCPServer#addr` / `TCPSocket#addr` / `#peeraddr` shape (no reverse DNS: the
/// host field carries the numeric IP).
pub fn tcp_addr(v: &Value, peer: bool) -> Result<Value, String> {
    let id = io_id(v).ok_or("not a socket")?;
    let addr: std::net::SocketAddr = with_host(|h| match h.io_handles.get(id as usize) {
        Some(IoCell::TcpListener {
            listener: Some(l), ..
        }) if !peer => l.local_addr().map_err(|e| e.to_string()),
        Some(IoCell::TcpStream {
            stream: Some(s), ..
        }) => {
            if peer {
                s.peer_addr().map_err(|e| e.to_string())
            } else {
                s.local_addr().map_err(|e| e.to_string())
            }
        }
        Some(IoCell::TcpListener { listener: None, .. })
        | Some(IoCell::TcpStream { stream: None, .. }) => Err("closed stream".to_string()),
        _ => Err("not a socket".to_string()),
    })?;
    let fam = if addr.is_ipv6() {
        "AF_INET6"
    } else {
        "AF_INET"
    };
    let ip = addr.ip().to_string();
    Ok(with_host(|h| {
        let items = vec![
            h.new_string(fam.to_string()),
            Value::Int(addr.port() as i64),
            h.new_string(ip.clone()),
            h.new_string(ip),
        ];
        h.new_array(items)
    }))
}

/// `TCPServer#close` / `TCPSocket#close`: drop the underlying handle (idempotent).
pub fn tcp_close(v: &Value) -> Result<(), String> {
    let id = io_id(v).ok_or("not a socket")?;
    with_host(|h| match h.io_handles.get_mut(id as usize) {
        Some(IoCell::TcpListener { listener, .. }) => *listener = None,
        Some(IoCell::TcpStream { stream, .. }) => *stream = None,
        _ => {}
    });
    Ok(())
}

/// `#closed?` for either socket kind.
pub fn tcp_closed(v: &Value) -> bool {
    match io_id(v) {
        Some(id) => with_host(|h| {
            matches!(
                h.io_handles.get(id as usize),
                Some(IoCell::TcpListener { listener: None, .. })
                    | Some(IoCell::TcpStream { stream: None, .. })
            )
        }),
        None => false,
    }
}

/// Like `call_proc`, but `self_override` (when given) rebinds `self` inside the
/// proc body — used for `define_method`, where the block runs as an instance
/// method with `self` = the receiver, yet still closes over its defining scope.
pub fn call_proc_self(
    proc_val: &Value,
    args: &[Value],
    self_override: Option<&Value>,
) -> Result<Value, String> {
    let (template, scope, kind, is_lambda) = match with_host(|h| h.obj(proc_val).cloned()) {
        Some(RObj::Proc {
            template,
            scope,
            kind,
            is_lambda,
        }) => (template, scope, kind, is_lambda),
        // A bound `Method` used as a block/proc (`map(&obj.method(:m))`): re-dispatch
        // the stored method on its captured receiver, with the Kernel fallback so a
        // bound Kernel method (`method(:puts)`) works off the `main` object too.
        Some(RObj::Method { recv, name }) => {
            return crate::builtins::call_bound(&recv, &name, args, None);
        }
        // The native `cycle` generator body: driven with a yielder (`args[0]`),
        // it pushes the captured elements round and round. The yielder returns a
        // break signal once its limit is hit (`first(n)`/`take(n)`), which unwinds
        // this loop; an empty buffer yields nothing (finite, empty).
        Some(RObj::CycleProc(buf)) => {
            if buf.is_empty() {
                return Ok(Value::Undef);
            }
            let Some(yielder) = args.first().cloned() else {
                return Err("no yielder is available".to_string());
            };
            loop {
                for v in &buf {
                    crate::builtins::dispatch(&yielder, "<<", std::slice::from_ref(v), None)?;
                    if has_pending_signal() {
                        return Ok(Value::Undef);
                    }
                }
            }
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
        ProcKind::Around(idx) => {
            // A native around block: `yield` in an around handler re-runs the
            // intercepted method's original body once. Yield args are ignored —
            // the original runs with its own captured arguments (MRI around).
            return drive_around(idx);
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
                    .lock()
                    .unwrap()
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
                child.lock().unwrap().vars.insert(p.clone(), v);
            }
            let splat_end = bound.len().saturating_sub(after).max(si);
            let rest: Vec<Value> = bound
                .get(si..splat_end)
                .map(|s| s.to_vec())
                .unwrap_or_default();
            let arr = with_host(|h| h.new_array(rest));
            child
                .lock()
                .unwrap()
                .vars
                .insert(def.params[si].clone(), arr);
            for (j, p) in def.params.iter().skip(si + 1).enumerate() {
                let v = bound.get(splat_end + j).cloned().unwrap_or(Value::Undef);
                child.lock().unwrap().vars.insert(p.clone(), v);
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
        // In a lambda, `return` and `break` are local — they end the lambda and
        // become its value (MRI lambda semantics). In a plain block/proc both
        // keep propagating to the defining method / enclosing loop.
        Some(Signal::Return(v) | Signal::Break(v)) if is_lambda => Ok(v),
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

/// Compile-time guard: `RubyHost` must stay `Send` — the GVL model shares one
/// host across `Thread`s. If a future field reintroduces `Rc`/a raw pointer/a
/// non-`Send` handle, this fails to compile (fix the field, don't delete this).
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<RubyHost>();
};
