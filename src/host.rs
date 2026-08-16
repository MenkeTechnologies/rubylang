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
    /// A unique id for the method activation this scope belongs to. A block
    /// captures its defining scope's `frame_id` as its "home"; a non-local
    /// `return` from that block unwinds to the method frame with this id (MRI
    /// block-return semantics), passing through any intermediate yielder frames.
    frame_id: u64,
}

thread_local! {
    static NEXT_FRAME_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}
/// Allocate a fresh, process-unique method-activation id (for `Scope::frame_id`).
fn next_frame_id() -> u64 {
    NEXT_FRAME_ID.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1));
        v
    })
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
                                      // A `break`/`next` inside a construct that compiles to its own chunk (a
                                      // `begin`/`rescue`) can only leave a *signal* behind — it cannot jump to a
                                      // native loop's exit, because that label lives in a different chunk. These
                                      // three let the enclosing native loop pick the signal back up right after
                                      // the nested chunk returns, so the jump happens in the chunk that owns it.
    pub const TAKE_LOOP_NEXT: u16 = 51; // [] -> Bool; consume a pending `next`
    pub const PEEK_LOOP_BREAK: u16 = 52; // [] -> Bool; is a `break` pending?
    pub const TAKE_LOOP_BREAK: u16 = 53; // [] -> the pending `break` value
                                         // `BEGIN` for a `begin` that sits directly in a native loop: identical, except
                                         // a `break`/`next` signal does NOT halt the chunk, so the handoff ops the
                                         // compiler emits right after it get to run and turn the signal into a jump.
    pub const BEGIN_IN_LOOP: u16 = 54; // [begin_id] -> run begin/rescue/ensure
    pub const SIG_REDO: u16 = 55; // [v] -> re-run the current block iteration
    pub const TAKE_LOOP_REDO: u16 = 56; // [] -> Bool; consume a pending `redo`
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
    /// `uniq` / `uniq { |x| key }` — passes an element through only the first
    /// time its key is seen. The optional block supplies the key; without one
    /// the element is its own key.
    ///
    /// Unlike every other stage this one is STATEFUL across elements, so its
    /// seen-set lives in `LazyState::Uniq` for the duration of one pull rather
    /// than in the op itself: the op is shared by every pull of the pipeline and
    /// must not accumulate between them.
    Uniq(Option<Value>),
}

/// How a derived generator reshapes the values its source yields. This is what
/// keeps a block-less enumerator method on an INFINITE source lazy: instead of
/// materializing the source and delegating to Array, the result is another
/// generator that applies `Derive` to the source's values on demand.
#[derive(Debug, Clone)]
pub enum Derive {
    /// Pass values through (block-less `each`/`map`/`select`/… all re-yield the
    /// element sequence unchanged).
    Each,
    /// `each_slice(n)` — consecutive groups of `n` (a short final group only
    /// once the source runs out).
    Slice(usize),
    /// `each_cons(n)` — every sliding window of `n`.
    Cons(usize),
    /// `each_with_index`/`with_index(offset)` — `[value, index]` pairs.
    WithIndex(i64),
    /// `each_with_object(obj)` — `[value, obj]` pairs.
    WithObject(Value),
}

/// A heap object — the Ruby reference types.
/// A `Generator`'s external-iteration state — MRI's `enumerator.c` fiber model.
///
/// The generator block runs inside `fiber`, suspending at every `y << v`, so
/// `Enumerator#next` advances it by exactly one element. `peeked` holds a value
/// that `peek` pulled off the fiber but `next` has not consumed yet (`peek` must
/// not advance). Created on the first `next`/`peek`, dropped by `rewind`.
#[derive(Debug, Clone)]
pub struct GenExt {
    pub fiber: Value,
    pub peeked: Option<Value>,
}

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
        /// `compare_by_identity` mode: keys hash/compare by object identity.
        by_identity: bool,
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
        /// The object `.lazy` was called on, which `inspect` shows. It differs
        /// from `source` whenever the receiver had to be materialized to feed
        /// the pipeline (a Hash, a Set, an Enumerator).
        origin: Value,
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
    /// A Range over arbitrary `<=>`-comparable objects (`IPAddr#to_range`, custom
    /// Comparable). Membership uses `<=>`; iteration uses `succ` (raising if the
    /// endpoints don't provide one, like MRI).
    ObjRange {
        lo: Value,
        hi: Value,
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
    /// A native generator body yielding `lo, lo+1, lo+2, …` forever: the driving
    /// block of the Enumerator an endless Range (`(1..)`) answers a block-less
    /// enumerator method with. Bounded by the consumer, like `CycleProc`.
    SeqProc(i64),
    /// A native generator body yielding `from`, `from + by`, `from + 2·by`, …
    /// forever: the driving block of the Enumerator a limitless `Numeric#step`
    /// (`1.step(by: 3)`) answers with. Bounded by the consumer, like `SeqProc`.
    /// Integer successors come from `+`, so the sequence keeps its receiver's
    /// numeric class and promotes on overflow exactly as MRI's does. `float`
    /// selects MRI's `ruby_float_step` formula instead — the i-th value is
    /// `i·by + from`, NOT the running sum, and the two disagree on accumulated
    /// error (`0.0.step(by: 0.1).first(7).last` is `0.6000000000000001`).
    StepProc {
        from: Value,
        by: Value,
        float: bool,
    },
    /// A native generator body that reshapes another generator's values. `src` is
    /// that source generator's own driving block (re-run from the start on each
    /// batch, as generator blocks are pure); `kind` is the transform.
    DeriveProc {
        src: Box<Value>,
        kind: Derive,
    },
    /// A bound `Method` object (`obj.method(:name)`): the captured receiver plus
    /// the method name. `#call(*args)` routes back through dispatch on the stored
    /// receiver; `#to_proc` yields a callable that closes over both.
    Method {
        recv: Value,
        name: String,
        /// An UnboundMethod (`Module#instance_method`, `Method#unbind`), whose
        /// `recv` is the class the method was looked up on rather than an
        /// object. Both forms store a class, so only this flag says whether a
        /// class receiver means "its class method" or "its instance method".
        unbound: bool,
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
        /// The literal Ruby flag letters present at construction (`i`, `m`, `x`),
        /// kept so `Regexp#options`/`#casefold?`/`#to_s` can report them (the
        /// compiled `re` bakes them in and can't be read back).
        flags: String,
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
        /// The object this Enumerator iterates, when it is known. `each` answers
        /// it (`[1, 2, 3, 4].each_slice(2).each { }` is `[1, 2, 3, 4]`, not the
        /// slices) and `inspect` shows it, both of which the buffer alone cannot
        /// reconstruct — `each_cons` windows overlap.
        source: Option<Value>,
    },
    /// A block-based generator (`Enumerator.new { |y| ... }`): the user block
    /// that drives it by sending `<<`/`yield` to a yielder. Bulk operations
    /// (`to_a`/`first`/`take`/`lazy`) re-run the block from the start each time
    /// (blocks are pure/re-runnable).
    ///
    /// External iteration (`next`/`peek`) instead runs the block on a `Fiber`,
    /// as MRI's `enumerator.c` does, so it advances exactly one `y << v` per
    /// `next` — see [`GenExt`]. `materialized` is the older buffer+cursor path,
    /// still used for an endless `cycle` generator, whose fiber would never end.
    Generator {
        block: Value,
        materialized: Option<(Vec<Value>, usize)>,
        ext: Option<GenExt>,
    },
    /// The native `Enumerator::Yielder` passed to a generator block as `|y|`.
    /// `<<`/`yield` push into the collector `enum_sinks[sink]`; once the buffer
    /// reaches `limit`, `<<`/`yield` raise a break signal to unwind the block,
    /// bounding infinite `loop {}`/`while` generators for `first(n)`/`take(n)`.
    Yielder {
        sink: usize,
        limit: usize,
    },
    /// The `Enumerator::Yielder` handed to a generator block that is running on
    /// an external-iteration `Fiber`: `<<`/`yield` suspend the fiber with the
    /// value instead of buffering it, so the block advances one element per
    /// `Enumerator#next`. No state of its own — `Fiber.yield` finds the running
    /// fiber through `CUR_FIBER`.
    FiberYielder,
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
    /// `Method#curry` — the same gathering, but the full call invokes the bound
    /// `target` Method rather than a proc template.
    MethodCurried {
        target: Box<Value>,
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
    /// Per-method visibility for the entries this class owns. Public is the
    /// default and is NOT stored, so an absent name means public — which keeps
    /// the map empty for the overwhelmingly common class and makes a reopening
    /// merge a plain extend. Keyed by method name, exactly like MRI's per-entry
    /// visibility (it belongs to the class the method is defined in, not to the
    /// method body, so an inherited method can be made private in a subclass).
    pub visibility: IndexMap<String, Visibility>,
    /// Per-CLASS-method visibility — what `private_class_method :m` records.
    ///
    /// A separate map from [`ClassDef::visibility`] because the two namespaces
    /// are separate in MRI too: the instance entry lives on the class, the class
    /// entry lives on its singleton class, and `private :m` / `private_class_method :m`
    /// on the same name are independent facts. Storing both in one map would let
    /// `private :run` silently hide `self.run`.
    ///
    /// Public is the default and is not stored, so an absent name is public and
    /// the map stays empty for a class that never restricts a class method.
    pub class_visibility: IndexMap<String, Visibility>,
    /// True when this was opened with `module`, not `class` (or created by
    /// `Module.new`). A module is an instance of `Module`, not of `Class`, so
    /// `M.class` is `Module`, `M.is_a?(Class)` is false, and `M` has no
    /// `superclass` — none of which is derivable from the rest of the def, since
    /// a module and a superclass-less class look identical otherwise.
    pub is_module: bool,
}

/// MRI method visibility. Only the two non-default values are ever stored; see
/// [`ClassDef::visibility`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Visibility {
    #[default]
    Public,
    Private,
    Protected,
}

impl Visibility {
    /// The compact form stored in the bytecode cache tuple.
    pub fn as_u8(self) -> u8 {
        match self {
            Visibility::Public => 0,
            Visibility::Private => 1,
            Visibility::Protected => 2,
        }
    }
    /// Inverse of [`Visibility::as_u8`]; an unknown byte reads as public, so a
    /// cache entry from a future writer degrades instead of panicking.
    pub fn from_u8(b: u8) -> Self {
        match b {
            1 => Visibility::Private,
            2 => Visibility::Protected,
            _ => Visibility::Public,
        }
    }
    /// The word MRI puts in `NoMethodError`: `private method 'm' called for …`.
    pub fn word(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Protected => "protected",
        }
    }
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

/// Ruby's `Regexp#options` bitmask for a regexp's flag text: `IGNORECASE` 1,
/// `EXTENDED` 2, `MULTILINE` 4.
///
/// This is what decides whether two Regexps are the same value, so it must be
/// computed from the bits and never from the flag TEXT: a literal records the
/// letters in the order they were written, so `/a/im` and `/a/mi` carry
/// different text and identical options, and Ruby calls them equal.
pub fn regex_option_bits(flags: &str) -> u8 {
    let mut bits = 0u8;
    if flags.contains('i') {
        bits |= 1;
    }
    if flags.contains('x') {
        bits |= 2;
    }
    if flags.contains('m') {
        bits |= 4;
    }
    bits
}

thread_local! {
    /// Container heap-id pairs whose structural `==` is currently being decided.
    /// A cycle re-enters the same pair, and MRI answers true for it rather than
    /// recursing; without this the native stack overflows and the process
    /// aborts, which no `rescue` can catch. Thread-local because each Ruby
    /// `Thread` runs its own comparisons.
    static EQ_PAIRS: std::cell::RefCell<Vec<(u32, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The same, for `eql?`. Deliberately a SEPARATE stack: `eql?` falls through
    /// to `==` for anything that is neither a number nor a container, so one
    /// shared stack would let an in-flight `eql?` answer the `==` it delegates
    /// to — which made `class Z; def ==(o); true; end; end; Z.new.eql?(Z.new)`
    /// report true where MRI reports false.
    static EQL_PAIRS: std::cell::RefCell<Vec<(u32, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A hashable Ruby value used as a Hash key. `Ord` is derived purely to give
/// the order-independent containers (`Hash`, `Set`) a canonical element order
/// to sort into; it is not a Ruby-visible ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// A Hash used as a Hash key (`{{a: 1} => v}`), keyed by its entries. Ruby's
    /// `Hash#hash` is order-independent (`{a: 1, b: 2}.hash == {b: 2, a: 1}.hash`),
    /// so the pairs are sorted into a canonical order before keying.
    Hash(Vec<(RKey, RKey)>),
    /// A Set used as a Hash key, keyed by its members — likewise sorted, since
    /// a Set is unordered for equality and hashing.
    Set(Vec<RKey>),
    /// An Integer too large for `i64`. A promoted BigInt never holds an
    /// in-range value, so it can never collide with `Int`.
    Big(num_bigint::BigInt),
    /// A Rational, keyed by its lowest-terms value.
    Rational(num_rational::BigRational),
    /// A Complex, keyed by both parts (each of which keeps its own numeric
    /// class, so `Complex(1, 0)` and `Complex(1.0, 0)` are distinct keys).
    Complex(Box<RKey>, Box<RKey>),
    /// A Range used as a Hash key: `(lo, hi, exclusive)` for an Integer range,
    /// or the String/Float endpoint variants.
    Range(i64, i64, bool),
    StrRange(String, String, bool),
    FloatRange(u64, u64, bool),
    /// A Regexp, keyed by `(source, options)` — the same pair `==` compares —
    /// so `/a/` and `/a/` are one Hash key and collapse under `uniq`.
    Regexp(String, u8),
    /// An identity key: the heap-object index of a `Value::Obj`, used when a Hash
    /// is in `compare_by_identity` mode so distinct-but-equal objects (two `[1]`
    /// arrays, two `"ab"` strings) hash as separate keys.
    Identity(u32),
    /// The stand-in for a container that reaches itself. `[1, a].hash` where
    /// `a` is that very array cannot key its own elements, and MRI still answers
    /// a finite Integer; keying the re-entry as this constant terminates the
    /// walk. Distinct from every other variant, so a recursive container never
    /// collides with a non-recursive one.
    Recursive,
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
    /// Arity, for the `ArgumentError` MRI raises before the body ever runs.
    /// `req` counts positional params with no default (on both sides of a splat);
    /// `opt` counts the ones with a default; `kwreq` names the keyword params
    /// with no default, in declaration order. A method built by the compiler for
    /// an `attr_*` accessor fills these in the same way a `def` does.
    pub req: u16,
    pub opt: u16,
    pub kwreq: Vec<String>,
    pub chunk: Chunk,
    /// Number of leading positional params bound directly into the body's fusevm
    /// frame slots `0..slot_params` (native-lowerable) instead of the host env.
    /// Non-zero only for a simple signature (all required positional, no defaults/
    /// splat/keyword/block, none captured by a closure) whose body slot-lowers;
    /// the dispatcher seeds those slots with the call's positional args before
    /// running the chunk. 0 = every param is host-bound as usual.
    pub slot_params: u16,
}

/// What a `Method`/`UnboundMethod` name resolved to, and the module that owns
/// it — the one answer `#arity`, `#owner` and `#parameters` all read.
pub enum MethodShape {
    /// A written `def` (or `attr_*`), and the class/module it was defined in.
    /// Boxed: a `MethodDef` carries a whole compiled chunk, and the other two
    /// variants are a handful of words.
    Def { def: Box<MethodDef>, owner: String },
    /// A `define_method` body (a proc template), and its owner.
    Block { template: usize, owner: String },
    /// A built-in, described by its row in the generated MRI shape table.
    Builtin {
        owner: &'static str,
        arity: i16,
        params: &'static str,
    },
}

/// A compiled block template.
#[derive(Clone)]
pub struct ProcDef {
    pub params: Vec<String>,
    /// Index of a `*rest` splat parameter, if any.
    pub splat: Option<usize>,
    pub chunk: Chunk,
    /// The parameter shape as written. A plain block ignores it (blocks bind
    /// leniently), but a lambda — `->`, `lambda { }`, `Method#to_proc`, a
    /// `define_method` body — is arity-checked against it exactly like a `def`,
    /// and `Proc#arity` is computed from it for both.
    pub arity: crate::ast::BlockArity,
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
    /// `return` value plus an optional target frame id. `None` is a local
    /// method-body return (consumed by the enclosing `run_method`); `Some(id)` is
    /// a block's non-local return targeting the method activation with that
    /// `frame_id` — intermediate frames re-propagate it until they match.
    Return(Value, Option<u64>),
    /// `retry` inside a `rescue` clause — restarts the enclosing `begin` body.
    Retry,
    /// `redo` — re-runs the current block iteration (or native loop body) from
    /// the top. Unlike `next`, the iterator does not advance and the loop
    /// condition is not re-tested; unlike `retry`, block-locals keep their
    /// values across the re-run (MRI: `[10].each { y = (y||0)+1; redo if … }`
    /// counts 1, 2, 3).
    Redo,
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
    /// Arrays that are the PACKED form of a multi-value `y.yield a, b`, by object
    /// id. The pack is an ordinary Array everywhere else; this only records that
    /// the iteration yielded two values, which decides how a block binds them
    /// (see `multi_yield_consume`). Object ids are never reused, so an id here
    /// always names the same pack.
    multi_yield_packs: HashSet<u32>,
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
    /// `require` names of the stdlib bundled into the binary (`uri`, `csv`,
    /// `rubygems/version`, …) that have already been compiled and run on this
    /// host. They have no path on disk, so they dedup here instead of through
    /// `$LOADED_FEATURES` — that Array holds the paths the *program* required.
    embedded_stdlib_loaded: std::collections::HashSet<String>,
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
    /// `alias_method`s whose target is a native attr accessor, mapping the alias
    /// method name (with any trailing `=`) to the underlying `(field, is_writer)`.
    /// ActiveSupport's `attr_internal` builds `view_runtime`/`view_runtime=` this
    /// way: `attr_writer :_view_runtime; alias_method :view_runtime=, :_view_runtime=`.
    attr_aliases: IndexMap<String, IndexMap<String, (String, bool)>>,
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
    /// Instance variables for heap objects that aren't plain `RObj::Object`
    /// (Thread, Fiber, IO, and other native-handle values), keyed by heap id.
    /// ActiveSupport reopens `Thread`/`Fiber` with `attr_accessor
    /// :active_support_execution_state`; without somewhere to store the ivar the
    /// accessor silently drops the value and every request loses its execution
    /// state. `RObj::Object` keeps its own inline `ivars`; this covers the rest.
    obj_ivars: IndexMap<u32, IndexMap<String, Value>>,
    /// `alias_method`/`alias` mappings: class → alias name → target method name.
    method_aliases: IndexMap<String, IndexMap<String, String>>,
    /// For an alias whose target is a USER method (the body is copied under the
    /// alias name): class → alias name → original method name. `super` from the
    /// alias must resolve as the original (Ruby aliases preserve the super
    /// binding): `alias raw_request_method request_method`, whose copied body does
    /// `check_method(super)`, must find `request_method`'s super, not the
    /// non-existent `raw_request_method` super.
    alias_originals: IndexMap<String, IndexMap<String, String>>,
    /// Modules that ran a bare `module_function` at runtime: their instance
    /// methods are also callable as module (class) methods. The compile-time path
    /// promotes direct class-body `def`s, but a `def` nested in an `if`/`else`
    /// (rack's `Rack::Utils.escape_html`) is defined at runtime — this set lets
    /// class-method dispatch fall back to such an instance method.
    module_function_modules: std::collections::HashSet<String>,
    /// Class override for a native-backed instance of a *user subclass of a
    /// builtin collection* (`class Params < Hash`): heap id → user class name.
    /// The value stays a native `RObj::Hash`/`Array`/`Str` so builtin ops work
    /// unchanged, while `class_of`/`is_a?`/method resolution report the subclass
    /// so its own methods and `#class` behave like MRI.
    class_overrides: IndexMap<u32, String>,
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
    /// In-process output sink. When `Some`, everything the program writes to the
    /// native stdout/stderr streams is appended here instead of reaching the
    /// process — what an embedder that owns the terminal (a TUI) needs so a
    /// `puts` cannot corrupt its display. `None` (the default) is the ordinary
    /// standalone `ruby` behaviour.
    capture: Option<String>,
    /// Heap ids of the containers whose `to_s`/`inspect` rendering is currently
    /// on the stack. A container that (directly or through a cycle) holds
    /// itself would otherwise recurse until the native stack overflows and the
    /// process aborts — an abort no `rescue` can catch. MRI instead elides the
    /// re-entry (`[1, [...]]`, `{a: {...}}`, `Set[Set[...]]`,
    /// `#<struct S a=#<struct S:...>>`), which is what `cycle_marker` renders.
    /// A stack, not a set: `[x, x]` renders `x` twice because the first render
    /// pops before the second begins; only genuine re-entry elides.
    rendering: Vec<u32>,
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
                frame_id: next_frame_id(),
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
                frame_id: next_frame_id(),
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
    // A `def` at the top level of the eval'd source belongs to the active eval
    // target — `C.class_eval("def m; … end")` defines `m` on C, not as a global
    // top-level (Object) method. Without this, activesupport's `class_eval
    // ("def warn …")` registered `warn` globally and shadowed `Kernel#warn`,
    // which then routed every bare `warn` into a deprecation path.
    let target = DEF_TARGET.with(|t| t.borrow().last().cloned());
    with_host(|h| {
        for (name, def) in prog.methods {
            // Synthetic bodies (`__def123__` stashes for a runtime `def obj.m` /
            // `class << self; def m`, `__class_body__N`) are looked up by name in
            // the top-level table by their DEFINE op — they must stay there. Only
            // real method names follow the eval target.
            let synthetic = name.starts_with("__");
            match &target {
                Some(DefTarget::Instance(c)) if !synthetic => h.add_instance_method(c, &name, def),
                Some(DefTarget::ClassMethod(c)) if !synthetic => h.add_class_method(c, &name, def),
                Some(DefTarget::Singleton(id)) if !synthetic => {
                    h.add_singleton_method(*id, &name, def)
                }
                _ => {
                    h.methods.insert(name, def);
                }
            }
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
    // Module-ness is a property of the first opening: a `class Foo` that later
    // sees a stray `module Foo` is a TypeError in MRI, not a demotion, and the
    // common case here is a reopening that agrees. Only ever set the flag, so a
    // `module M` opened before a merge of an unrelated (default) def keeps it.
    existing.is_module |= def.is_module;
    for (k, v) in def.visibility {
        existing.visibility.insert(k, v);
    }
    for (k, v) in def.class_visibility {
        existing.class_visibility.insert(k, v);
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

/// The heap slot `main` — the top-level `self` — always occupies. It is the
/// first thing `RubyHost::new` puts on the heap and nothing ever displaces it.
pub const MAIN_OBJ_ID: u32 = 0;

/// Is this the top-level `main` object?
///
/// `main` is an ordinary `Object` (so `self.class` is `Object`), but MRI gives
/// it a singleton `to_s`/`inspect` answering `"main"`, and names it `main` in a
/// `NoMethodError` rather than `an instance of Object`:
///
/// ```text
/// $ /opt/homebrew/opt/ruby/bin/ruby -e 'p self'      # main
/// $ /opt/homebrew/opt/ruby/bin/ruby -e 'self.nope'
/// -e:1:in '<main>': undefined method 'nope' for main (NoMethodError)
/// ```
pub fn is_main(v: &Value) -> bool {
    matches!(v, Value::Obj(id) if *id == MAIN_OBJ_ID)
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
                    frame_id: next_frame_id(),
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
            rendering: Vec::new(),
            multi_yield_packs: HashSet::new(),
            around_stack: Vec::new(),
            threads: Vec::new(),
            queues: Vec::new(),
            condvars: Vec::new(),
            io_handles: vec![IoCell::Stdout, IoCell::Stderr, IoCell::Stdin],
            db_handles: Vec::new(),
            fiddle_libs: Vec::new(),
            fiddle_mem: Vec::new(),
            capture: None,
            struct_defs: IndexMap::new(),
            data_classes: std::collections::HashSet::new(),
            embedded_stdlib_loaded: std::collections::HashSet::new(),
            struct_counter: 0,
            class_vars: IndexMap::new(),
            class_ivars: IndexMap::new(),
            attr_accessors: IndexMap::new(),
            attr_aliases: IndexMap::new(),
            define_methods: IndexMap::new(),
            singleton_methods: IndexMap::new(),
            singleton_define_methods: IndexMap::new(),
            class_define_methods: IndexMap::new(),
            obj_ivars: IndexMap::new(),
            method_aliases: IndexMap::new(),
            alias_originals: IndexMap::new(),
            module_function_modules: std::collections::HashSet::new(),
            class_overrides: IndexMap::new(),
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
                        Some(
                            RObj::Range { .. }
                                | RObj::FloatRange { .. }
                                | RObj::StrRange { .. }
                                | RObj::ObjRange { .. }
                        )
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
                let new = self.alloc(copy);
                // Preserve a native-backed builtin-subclass override (`class
                // Params < Hash`): the copy must report the same class, or a
                // `dup`/`merge` on a `Sinatra::IndifferentHash` degrades to a
                // plain Hash and loses its indifferent (symbol/string) access.
                if let Value::Obj(oid) = v {
                    if let Some(cls) = self.class_overrides.get(oid).cloned() {
                        self.set_class_override(&new, &cls);
                    }
                }
                new
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
            by_identity: false,
        })
    }
    /// `Hash.new(default)` — a hash whose `[]` returns `default` for absent keys.
    pub fn new_hash_with_default(&mut self, map: IndexMap<RKey, Value>, default: Value) -> Value {
        self.alloc(RObj::Hash {
            map,
            default,
            default_proc: None,
            by_identity: false,
        })
    }
    /// Set the value returned for missing keys (`Hash#default=`), in place.
    pub fn set_hash_default(&mut self, v: &Value, default: Value) {
        if let Some(RObj::Hash { default: d, .. }) = self.obj_mut(v) {
            *d = default;
        }
    }
    /// `Hash#default_proc=` — set (or clear, with nil) the miss block.
    pub fn set_hash_default_proc(&mut self, v: &Value, proc: Value) {
        if let Some(RObj::Hash {
            default_proc: p, ..
        }) = self.obj_mut(v)
        {
            *p = if matches!(proc, Value::Undef) {
                None
            } else {
                Some(proc)
            };
        }
    }
    /// `Hash.new { |h,k| ... }` — a hash whose `[]` calls the block on a miss.
    pub fn new_hash_with_proc(&mut self, map: IndexMap<RKey, Value>, proc: Value) -> Value {
        self.alloc(RObj::Hash {
            map,
            default: Value::Undef,
            default_proc: Some(proc),
            by_identity: false,
        })
    }
    /// Enable `compare_by_identity` on a hash (in place). MRI also re-keys any
    /// existing entries, but the idiom (and the only caller that matters — the
    /// Journey GTG builder) sets it on a fresh empty hash, so subsequent inserts
    /// pick up identity keying via `hash_key`.
    pub fn set_hash_by_identity(&mut self, v: &Value) {
        if let Some(RObj::Hash { by_identity, .. }) = self.obj_mut(v) {
            *by_identity = true;
        }
    }
    /// Whether a hash is in `compare_by_identity` mode.
    pub fn hash_is_by_identity(&self, v: &Value) -> bool {
        matches!(
            self.obj(v),
            Some(RObj::Hash {
                by_identity: true,
                ..
            })
        )
    }
    /// Build a Hash key for `v` against the receiver hash's identity mode.
    pub fn hash_key(&self, recv: &Value, v: &Value) -> RKey {
        if self.hash_is_by_identity(recv) {
            if let Value::Obj(i) = v {
                return RKey::Identity(*i);
            }
        }
        self.to_key(v)
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
        let origin = source.clone();
        self.alloc(RObj::Lazy {
            source,
            ops,
            origin,
        })
    }
    /// As [`new_lazy`], for a receiver that had to be materialized: `origin` is
    /// what `.lazy` was called on, `source` what the pipeline pulls from.
    pub fn new_lazy_of(&mut self, source: Value, ops: Vec<LazyOp>, origin: Value) -> Value {
        self.alloc(RObj::Lazy {
            source,
            ops,
            origin,
        })
    }
    /// The object a lazy enumerator was built from (`Enumerator::Lazy#inspect`).
    pub fn lazy_origin(&self, v: &Value) -> Option<Value> {
        match self.obj(v) {
            Some(RObj::Lazy { origin, .. }) => Some(origin.clone()),
            _ => None,
        }
    }
    /// How `inspect` names a pipeline stage. The argument-taking ones show their
    /// argument (`take(2)`, `zip([3, 4])`), matching MRI.
    fn lazy_op_tag(&mut self, op: &LazyOp) -> String {
        match op {
            LazyOp::Map(_) => "map".to_string(),
            LazyOp::Select(_) => "select".to_string(),
            LazyOp::Reject(_) => "reject".to_string(),
            LazyOp::FilterMap(_) => "filter_map".to_string(),
            LazyOp::FlatMap(_) => "flat_map".to_string(),
            LazyOp::TakeWhile(_) => "take_while".to_string(),
            LazyOp::DropWhile(_) => "drop_while".to_string(),
            LazyOp::Take(n) => format!("take({n})"),
            LazyOp::Drop(n) => format!("drop({n})"),
            LazyOp::Zip(others) => {
                let args: Vec<String> = others.iter().map(|xs| self.inspect_array(xs)).collect();
                format!("zip({})", args.join(", "))
            }
            // MRI tags it `uniq` whether or not a key block was given — the
            // block is not shown, so both forms inspect identically.
            LazyOp::Uniq(_) => "uniq".to_string(),
        }
    }
    /// The `(source, ops)` of a lazy enumerator, if `v` is one.
    pub fn lazy_parts(&self, v: &Value) -> Option<(Value, Vec<LazyOp>)> {
        match self.obj(v) {
            Some(RObj::Lazy { source, ops, .. }) => Some((source.clone(), ops.clone())),
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
            source: None,
        })
    }
    /// As [`new_enumerator`], recording the object being iterated.
    pub fn new_enumerator_of(&mut self, buf: Vec<Value>, method: &str, source: Value) -> Value {
        self.alloc(RObj::Enumerator {
            buf,
            cursor: 0,
            method: method.to_string(),
            source: Some(source),
        })
    }
    /// Re-point an Enumerator at the object it really iterates. A non-Array
    /// enumerable is materialized to an Array before the Array dispatcher builds
    /// the Enumerator, so the source it recorded is that temporary.
    pub fn set_enum_source(&mut self, v: &Value, src: Value) {
        if let Some(RObj::Enumerator { source, .. }) = self.obj_mut(v) {
            *source = Some(src);
        }
    }
    /// The object an Enumerator iterates, when it recorded one.
    pub fn enum_source(&self, v: &Value) -> Option<Value> {
        match self.obj(v) {
            Some(RObj::Enumerator { source, .. }) => source.clone(),
            _ => None,
        }
    }
    /// Build a block-based generator (`Enumerator.new { |y| ... }`).
    pub fn new_generator(&mut self, block: Value) -> Value {
        self.alloc(RObj::Generator {
            block,
            materialized: None,
            ext: None,
        })
    }
    /// The `Enumerator::Yielder` for a generator running on an external-iteration
    /// fiber (its `<<` suspends the fiber instead of buffering).
    pub fn new_fiber_yielder(&mut self) -> Value {
        self.alloc(RObj::FiberYielder)
    }
    /// Whether `v` is that fiber-backed yielder, so `<<` should suspend.
    pub fn is_fiber_yielder(&self, v: &Value) -> bool {
        matches!(self.obj(v), Some(RObj::FiberYielder))
    }
    /// The external-iteration fiber of a `Generator`, if one has been started.
    pub fn generator_ext_fiber(&self, v: &Value) -> Option<Value> {
        match self.obj(v) {
            Some(RObj::Generator { ext: Some(e), .. }) => Some(e.fiber.clone()),
            _ => None,
        }
    }
    /// Start external iteration on `fiber` (the first `next`/`peek`).
    pub fn generator_ext_start(&mut self, v: &Value, fiber: Value) {
        if let Some(RObj::Generator { ext, .. }) = self.obj_mut(v) {
            *ext = Some(GenExt {
                fiber,
                peeked: None,
            });
        }
    }
    /// The value a previous `peek` pulled but no `next` has consumed yet.
    pub fn generator_peeked(&self, v: &Value) -> Option<Value> {
        match self.obj(v) {
            Some(RObj::Generator { ext: Some(e), .. }) => e.peeked.clone(),
            _ => None,
        }
    }
    pub fn generator_set_peeked(&mut self, v: &Value, val: Option<Value>) {
        if let Some(RObj::Generator { ext: Some(e), .. }) = self.obj_mut(v) {
            e.peeked = val;
        }
    }
    /// A block-less endless `cycle` Enumerator: a `Generator` whose body is a
    /// native `CycleProc` repeating `buf` forever (bounded by the consumer via
    /// `first(n)`/`take(n)`).
    pub fn new_cycle_enumerator(&mut self, buf: Vec<Value>) -> Value {
        let block = self.alloc(RObj::CycleProc(buf));
        self.new_generator(block)
    }
    /// The Enumerator a block-less enumerator method on a generator answers
    /// with: the source's own driving block, reshaped by `kind`. Nothing is
    /// pulled here, so an infinite source stays infinite.
    pub fn new_derived_enumerator(&mut self, src_block: Value, kind: Derive) -> Value {
        let block = match kind {
            Derive::Each => src_block,
            kind => self.alloc(RObj::DeriveProc {
                src: Box::new(src_block),
                kind,
            }),
        };
        self.new_generator(block)
    }
    /// The same, for an endless Range: its element sequence is `lo, lo+1, …`.
    pub fn new_endless_range_enumerator(&mut self, lo: i64, kind: Derive) -> Value {
        let seq = self.alloc(RObj::SeqProc(lo));
        self.new_derived_enumerator(seq, kind)
    }
    /// The Enumerator a limitless `Numeric#step` answers with: `from`,
    /// `from + by`, … forever, materialized only as far as a consumer pulls.
    pub fn new_step_enumerator(&mut self, from: Value, by: Value, float: bool) -> Value {
        let seq = self.alloc(RObj::StepProc { from, by, float });
        self.new_generator(seq)
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
            materialized, ext, ..
        }) = self.obj_mut(v)
        {
            if let Some((_, cursor)) = materialized {
                *cursor = 0;
            }
            // Drop the external-iteration fiber: the next `next`/`peek` starts a
            // fresh one, re-running the block from the top (MRI `rewind`).
            *ext = None;
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
    /// Any real number (Integer, Float, BigInt, Rational) as `f64`, or `None`
    /// for a non-numeric value.
    pub fn as_f64(&self, v: &Value) -> Option<f64> {
        use num_traits::ToPrimitive as _;
        match v {
            Value::Int(n) => Some(*n as f64),
            Value::Float(f) => Some(*f),
            Value::Obj(_) => match self.obj(v) {
                Some(RObj::BigInt(b)) => b.to_f64(),
                Some(RObj::Rational(r)) => r.to_f64(),
                _ => None,
            },
            _ => None,
        }
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
    pub fn new_obj_range(&mut self, lo: Value, hi: Value, exclusive: bool) -> Value {
        self.alloc(RObj::ObjRange { lo, hi, exclusive })
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
    pub fn as_obj_range(&self, v: &Value) -> Option<(Value, Value, bool)> {
        match self.obj(v) {
            Some(RObj::ObjRange { lo, hi, exclusive }) => {
                Some((lo.clone(), hi.clone(), *exclusive))
            }
            _ => None,
        }
    }
    /// Compile a regex literal (Ruby `flags` → inline flags: `i`
    /// case-insensitive, `m` dot-matches-newline, `x` extended). Returns an error
    /// string if the pattern is not valid for the fancy-regex engine.
    ///
    /// The `flags` text is stored as written, so use [`regex_option_bits`] —
    /// never the string itself — to compare two Regexps' options.
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
                flags: flags.chars().filter(|c| "imx".contains(*c)).collect(),
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
            Some(RObj::Regexp { re, source, .. }) => Some((re.clone(), source.clone())),
            _ => None,
        }
    }
    /// The Ruby flag letters (`imx`) a Regexp was built with, for `#options` etc.
    pub fn regex_flags(&self, v: &Value) -> Option<String> {
        match self.obj(v) {
            Some(RObj::Regexp { flags, .. }) => Some(flags.clone()),
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
    /// Ruby `Proc#arity`. A curried proc reports `-1`; a normal proc reports the
    /// count MRI's `rb_proc_arity` derives from its written parameter shape,
    /// which is stricter for a lambda than for a plain block.
    pub fn proc_arity(&self, v: &Value) -> Option<i64> {
        match self.obj(v) {
            Some(RObj::Proc {
                kind,
                template,
                is_lambda,
                ..
            }) => match kind {
                ProcKind::Curried { .. }
                | ProcKind::MethodCurried { .. }
                | ProcKind::Composed { .. } => Some(-1),
                ProcKind::Collect(_) | ProcKind::Around(_) => Some(1),
                ProcKind::Normal => {
                    Some(ArityFacts::of_proc(&self.procs[*template]).arity_value(*is_lambda))
                }
            },
            // A `Symbol#to_proc` proc takes the receiver plus the method's own
            // arguments (MRI reports `-2`); a bound `Method` used as a proc
            // reports the method's arity.
            Some(RObj::SymProc(_)) => Some(-2),
            Some(RObj::Method {
                recv,
                name,
                unbound,
            }) => Some(self.method_arity(recv, name, *unbound)),
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
                    ProcKind::MethodCurried { .. }
                    | ProcKind::Composed { .. }
                    | ProcKind::Collect(_)
                    | ProcKind::Around(_) => return Some(v.clone()),
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
    /// Build the curried view of a bound `Method`: gathers `arity` args across
    /// successive calls, then invokes the method.
    pub fn new_method_curry(&mut self, target: Value, arity: usize) -> Value {
        let scope = self.cur_scope().clone();
        self.alloc(RObj::Proc {
            template: 0,
            scope,
            is_lambda: true,
            kind: ProcKind::MethodCurried {
                target: Box::new(target),
                arity,
                collected: Vec::new(),
            },
        })
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
    /// Record that `v` is the packed form of a multi-value yield (`y.yield a, b`).
    /// It stays an ordinary Array; only the fact that the iteration produced TWO
    /// values is remembered, which is what decides how a block binds them.
    pub fn mark_multi_yield(&mut self, v: &Value) {
        if let Value::Obj(id) = v {
            self.multi_yield_packs.insert(*id);
        }
    }
    /// Whether `v` is such a pack.
    pub fn is_multi_yield(&self, v: &Value) -> bool {
        matches!(v, Value::Obj(id) if self.multi_yield_packs.contains(id))
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
            unbound: false,
        })
    }
    /// Allocate an `UnboundMethod` (`Module#instance_method`, `Method#unbind`):
    /// `owner` is the class the method is looked up on, not a receiver.
    pub fn new_unbound_method(&mut self, owner: Value, name: &str) -> Value {
        self.alloc(RObj::Method {
            recv: owner,
            name: name.to_string(),
            unbound: true,
        })
    }
    /// The (receiver, method-name) of a bound `Method` value (`None` otherwise).
    pub fn as_method(&self, v: &Value) -> Option<(Value, String)> {
        match self.obj(v) {
            Some(RObj::Method { recv, name, .. }) => Some((recv.clone(), name.clone())),
            _ => None,
        }
    }
    /// Whether a `Method` value is an UnboundMethod — its stored receiver names
    /// the class to look the method up on as an INSTANCE method.
    pub fn is_unbound_method(&self, v: &Value) -> bool {
        matches!(self.obj(v), Some(RObj::Method { unbound: true, .. }))
    }
    /// The class a `Method`/`UnboundMethod` lookup of `name` resolves against —
    /// the name MRI reports in `undefined method 'x' for class '…'`. An instance
    /// receiver answers its own class; a class receiver answers itself for an
    /// UnboundMethod (`Foo.instance_method` names an INSTANCE method) and its
    /// singleton otherwise (`Foo.method` names a class method).
    /// The word MRI uses for `name` in an error message — `module` for a
    /// `module M`, `class` for everything else (`undefined method 'x' for
    /// module M` vs `… for class C`).
    pub fn class_or_module_word(&self, name: &str) -> &'static str {
        if self.is_module_name(name) {
            "module"
        } else {
            "class"
        }
    }
    pub fn method_lookup_class(&self, recv: &Value, unbound: bool) -> String {
        match self.classref_name(recv) {
            Some(cls) if unbound => cls,
            // A MODULE's singleton class is created lazily: until something
            // needs it, `M` is just an instance of `Module` and that is the
            // class MRI names. A CLASS is different — its singleton class
            // always exists, because `new`/`allocate` live there — so only the
            // module case is conditional. See [`Self::has_singleton_class`].
            Some(cls) if self.is_module_name(&cls) && !self.has_singleton_class(recv, &cls) => {
                "Module".to_string()
            }
            Some(cls) => format!("#<Class:{cls}>"),
            // `class_of`, not `dispatch_class`: the latter is the native-op
            // router and answers the RAW type for a builtin subclass, so a
            // `class Params < Hash` instance would name `Hash` here.
            None => self.class_of(recv),
        }
    }
    /// Whether a module's singleton class has been MATERIALISED.
    ///
    /// MRI does not create `#<Class:M>` when `module M; end` is evaluated; the
    /// module object is simply an instance of `Module`. The singleton class is
    /// created the first time something has to live in it, and until then MRI
    /// names `Module` as the lookup class:
    ///
    /// ```text
    /// module Plain; end
    /// Plain.method(:nope)   # NameError: … for class 'Module'
    ///
    /// module Owner; def self.x = 1; end
    /// Owner.method(:nope)   # NameError: … for class '#<Class:Owner>'
    /// ```
    ///
    /// Anything that puts an entry in the singleton class materialises it: a
    /// `def self.…`, an `extend` (the extended module joins the singleton
    /// ancestry), a `define_singleton_method`, or a singleton `def M.x`. A
    /// class is never lazy this way — `#<Class:C>` holds `new` and `allocate`
    /// from the start — so callers gate this on the receiver being a module.
    pub fn has_singleton_class(&self, recv: &Value, cls: &str) -> bool {
        if let Some(def) = self.classes.get(cls) {
            if !def.class_methods.is_empty() || !def.extends.is_empty() {
                return true;
            }
        }
        if self
            .class_define_methods
            .get(cls)
            .is_some_and(|m| !m.is_empty())
        {
            return true;
        }
        // A per-object singleton (`def M.x`, `M.define_singleton_method`) is
        // keyed by the module object's own id, not by its name.
        if let Value::Obj(id) = recv {
            if self
                .singleton_methods
                .get(id)
                .is_some_and(|m| !m.is_empty())
                || self
                    .singleton_define_methods
                    .get(id)
                    .is_some_and(|m| !m.is_empty())
            {
                return true;
            }
        }
        // An explicit `class << M` body opens the singleton class by name.
        self.classes.contains_key(&format!("#<Class:{cls}>"))
    }

    /// Whether the receiver actually HAS a method called `name` — the check
    /// `Object#method` and `Module#instance_method` gate on before handing back a
    /// `Method` object, so a name nothing defines raises `NameError` instead of
    /// yielding a `Method` that fails only when called.
    ///
    /// `resolve_method_shape` answers for written `def`s, `define_method` bodies
    /// and MRI's built-in table. It is not the whole surface: several kinds of
    /// definition live in tables of their own, and each is checked here so that a
    /// name dispatch WOULD resolve is never reported undefined —
    /// per-object singletons, an `alias` whose target is a built-in (which
    /// `find_method_owner` cannot resolve, as it only walks written defs),
    /// runtime `attr_*` accessors, and `Struct` members. Callers add
    /// `respond_to_missing?`, which has to run Ruby code and so cannot be
    /// answered from the host state alone.
    pub fn method_defined_on(&self, recv: &Value, name: &str, unbound: bool) -> bool {
        // A per-object singleton method belongs to the OBJECT, so only a bound
        // lookup sees it: an UnboundMethod names an instance method of a class.
        if !unbound
            && (self.find_singleton_method(recv, name).is_some()
                || self.find_singleton_define_method(recv, name).is_some())
        {
            return true;
        }
        match self.resolve_method_shape(recv, name, unbound) {
            // A bound lookup on a CLASS names a class method, but
            // `resolve_method_shape` deliberately falls back to the instance side
            // for one — rubylang stores the same class value for `Foo.method` and
            // `Foo.instance_method`, so the fallback keeps a mis-tagged lookup
            // answerable. Existence cannot inherit that leniency or
            // `String.method(:upcase)` reports defined where MRI raises, so
            // re-check against the singleton chain alone.
            Some(_) if !unbound && self.classref_name(recv).is_some() => {
                let cls = self.classref_name(recv).unwrap_or_default();
                if self.find_class_method_owner(&cls, name).is_some()
                    // A class IS an object, so the Object/Kernel instance side of
                    // its chain still counts — a top-level `def` (private on
                    // Object) is callable as `Foo.method(:helper)` in MRI.
                    || self.methods.contains_key(name)
                    || self.find_method_owner("Object", name).is_some()
                    || self
                        .singleton_lookup_chain(&cls)
                        .iter()
                        .any(|o| crate::arity_table::lookup(o, name).is_some())
                {
                    return true;
                }
            }
            Some(_) => return true,
            None => {}
        }
        // The classes whose own tables still have to be consulted. A class
        // receiver carries two: its singleton (where `def self.x` and
        // `singleton_class.attr_accessor` register) for a bound lookup, and the
        // class itself, whose instance-side tables an UnboundMethod names.
        // The singleton methods a `Struct.new` / `Data.define` class carries
        // itself. They are defined on the GENERATED class rather than on
        // `Struct`/`Data`, so dumping `Struct.methods` never sees them and no
        // table row describes them.
        if !unbound {
            if let Some(cls) = self.classref_name(recv) {
                let generated: &[&str] = if self.is_data_class(&cls) {
                    &["[]", "new", "members"]
                } else {
                    &["[]", "members", "keyword_init?"]
                };
                if self.struct_def(&cls).is_some() && generated.contains(&name) {
                    return true;
                }
            }
        }
        let mut chain: Vec<String> = Vec::new();
        if let Some(cls) = self.classref_name(recv) {
            // A bound lookup on a class sees only the SINGLETON side (`def self.x`,
            // `singleton_class.attr_accessor`); the class's instance methods are
            // not its class methods. An UnboundMethod names exactly the reverse.
            chain.push(if unbound {
                cls
            } else {
                format!("#<Class:{cls}>")
            });
        } else {
            // `class_of` rather than `object_class`, so a builtin subclass
            // (`class Params < Hash`) contributes its OWN name and its
            // `alias_method`/`attr_*` tables are consulted.
            chain.push(self.class_of(recv));
        }
        chain.iter().any(|cls| {
            // `resolve_method_shape` reaches the written-method table only via
            // `object_class`, which is None for a native-backed builtin subclass
            // (`class Aliased < Hash`) — so consult it here under `class_of`.
            self.find_method_owner(cls, name).is_some()
                || self.find_define_method(cls, name).is_some()
                || self.attr_access(cls, name).is_some()
                || self.find_alias(cls, name).is_some()
                || self.native_kernel_alias(cls, name).is_some()
                || self.struct_def(cls).is_some_and(|(members, _)| {
                    let member = name.strip_suffix('=').unwrap_or(name);
                    members.iter().any(|m| m == member)
                        || matches!(name, "deconstruct" | "deconstruct_keys")
                })
        })
    }
    /// Where a `Method`/`UnboundMethod` name resolved, and the module that owns
    /// it. `arity`/`owner`/`parameters` all read the same resolution, so they can
    /// never describe different methods.
    pub fn resolve_method_shape(
        &self,
        recv: &Value,
        name: &str,
        unbound: bool,
    ) -> Option<MethodShape> {
        if let Some(cls) = self.object_class(recv) {
            if let Some((def, owner)) = self.find_method_owner(&cls, name) {
                return Some(MethodShape::Def {
                    def: Box::new(def),
                    owner,
                });
            }
        } else if let Some(cls) = self.classref_name(recv) {
            // An UnboundMethod's class receiver names an INSTANCE method; a bound
            // one is a class method first, with the instance method as a fallback
            // (a plain `Foo.method(:bar)` and `Foo.instance_method(:bar)` store the
            // class identically).
            if unbound {
                if let Some((def, owner)) = self.find_method_owner(&cls, name) {
                    return Some(MethodShape::Def {
                        def: Box::new(def),
                        owner,
                    });
                }
            } else if let Some((def, owner)) = self.find_class_method_owner(&cls, name) {
                // A `def self.m` is owned by the defining class's SINGLETON class;
                // one reached through `extend M` is owned by `M` itself.
                let owner = if self
                    .classes
                    .get(&owner)
                    .is_some_and(|d| d.class_methods.contains_key(name))
                {
                    format!("#<Class:{owner}>")
                } else {
                    owner
                };
                return Some(MethodShape::Def {
                    def: Box::new(def),
                    owner,
                });
            }
            if let Some((def, owner)) = self.find_method_owner(&cls, name) {
                return Some(MethodShape::Def {
                    def: Box::new(def),
                    owner,
                });
            }
        }
        // A top-level `def` lives in the flat method table, not on `Object`, so
        // fall back to it before reporting "builtin". MRI owns those by `Object`.
        if let Some(def) = self.methods.get(name) {
            return Some(MethodShape::Def {
                def: Box::new(def.clone()),
                owner: "Object".to_string(),
            });
        }
        // A `define_method` body describes the method it defined: its block is
        // arity-checked with method (strict) semantics.
        if let Some(cls) = self.object_class(recv).or_else(|| self.classref_name(recv)) {
            if let Some(owner) = self.class_ancestry(&cls).into_iter().find(|c| {
                self.define_methods
                    .get(c)
                    .is_some_and(|m| m.contains_key(name))
            }) {
                let p = self.define_methods[&owner][name].clone();
                if let Some(RObj::Proc {
                    template,
                    kind: ProcKind::Normal,
                    ..
                }) = self.obj(&p)
                {
                    return Some(MethodShape::Block {
                        template: *template,
                        owner,
                    });
                }
            }
        }
        self.builtin_shape(recv, name, unbound)
    }
    /// The module the reference interpreter defines `name` on for `recv` — the
    /// first one in the receiver's ancestor chain with a table row. This is the
    /// key both `src/arity_table.rs` argument-shape tables are looked up by, so a
    /// call-time arity check resolves the method exactly as reflection does.
    pub fn builtin_owner(&self, recv: &Value, name: &str) -> Option<&'static str> {
        self.builtin_chain(recv, false)
            .iter()
            .find_map(|owner| crate::arity_table::lookup(owner, name).map(|(owner, _, _)| owner))
    }
    /// The built-in row describing `name` for `recv`: the first module in the
    /// receiver's ancestor chain that the reference interpreter defines it on.
    fn builtin_shape(&self, recv: &Value, name: &str, unbound: bool) -> Option<MethodShape> {
        let chain = self.builtin_chain(recv, unbound);
        chain.iter().find_map(|owner| {
            crate::arity_table::lookup(owner, name).map(|(owner, arity, params)| {
                MethodShape::Builtin {
                    owner,
                    arity,
                    params,
                }
            })
        })
    }
    /// The ancestor chain a built-in lookup for `recv` walks.
    fn builtin_chain(&self, recv: &Value, unbound: bool) -> Vec<String> {
        match self.classref_name(recv) {
            // An UnboundMethod on a class looks its name up as an INSTANCE method.
            Some(cls) if unbound => self.expanded_ancestry(&cls),
            // A bound class receiver is a class-method call (`Integer.sqrt`) first;
            // the instance chain behind it keeps a mis-tagged lookup answerable.
            Some(cls) => {
                let mut c = self.singleton_lookup_chain(&cls);
                c.extend(self.expanded_ancestry(&cls));
                c
            }
            None => self.expanded_ancestry(&self.dispatch_class(recv)),
        }
    }
    /// The ancestor chain to resolve a built-in method against: the runtime's own
    /// ancestry (which knows the user's classes, includes and prepends), with
    /// every built-in class in it expanded to the chain the reference interpreter
    /// reports — `class Foo < Array` must reach `Enumerable`, and `File` `IO`.
    pub fn expanded_ancestry(&self, class: &str) -> Vec<String> {
        let mut out = Vec::new();
        for a in self.class_ancestry(class) {
            match crate::arity_table::ancestry(&a) {
                Some(chain) => out.extend(chain.iter().map(|s| s.to_string())),
                None => out.push(a),
            }
        }
        dedup_keep_first(out)
    }
    /// The chain a method call on a CLASS resolves against: each class ancestor's
    /// singleton class (`#<Class:Integer>`, `#<Class:Numeric>`, …), then `Class`
    /// (a module has none), `Module`, and the common root — MRI's
    /// `Integer.singleton_class.ancestors`.
    fn singleton_lookup_chain(&self, class: &str) -> Vec<String> {
        let mut out = vec![format!("#<Class:{class}>")];
        // A module's singleton class inherits from `Module` directly — it has no
        // superclass singletons ahead of it, and no `Class`.
        if !self.is_module_name(class) {
            out.extend(
                self.expanded_ancestry(class)
                    .into_iter()
                    .skip(1)
                    .filter(|a| !self.is_module_name(a))
                    .map(|a| format!("#<Class:{a}>")),
            );
            out.push("Class".to_string());
        }
        out.extend(["Module", "Object", "Kernel", "BasicObject"].map(String::from));
        out
    }
    /// `Method#owner` — the class or module that DEFINES the method, which is
    /// rarely the receiver's own class (`3.method(:between?).owner` is
    /// `Comparable`). Falls back to the receiver's class for a method no table
    /// row and no definition describes.
    pub fn method_owner(&self, recv: &Value, name: &str, unbound: bool) -> String {
        match self.resolve_method_shape(recv, name, unbound) {
            Some(MethodShape::Def { owner, .. }) | Some(MethodShape::Block { owner, .. }) => owner,
            Some(MethodShape::Builtin { owner, .. }) => owner.to_string(),
            None => self
                .classref_name(recv)
                .map(|c| format!("#<Class:{c}>"))
                .unwrap_or_else(|| self.dispatch_class(recv)),
        }
    }
    /// `Method#parameters` descriptors: `(kind, name)` pairs, with the name absent
    /// for a built-in (native code has no written parameter names, and MRI reports
    /// those as one-element `[:req]` / `[:rest]` entries).
    pub fn method_parameters(
        &self,
        recv: &Value,
        name: &str,
        unbound: bool,
    ) -> Vec<(&'static str, Option<String>)> {
        match self.resolve_method_shape(recv, name, unbound) {
            Some(MethodShape::Def { def, .. }) => written_params(
                &def.params,
                def.splat,
                def.opt as usize,
                &def.kwparams,
                &def.kwreq,
                def.kwsplat.as_deref(),
                def.blockparam.as_deref(),
            ),
            Some(MethodShape::Block { template, .. }) => {
                let p = &self.procs[template];
                // The parser desugars a block's keyword params into one synthetic
                // trailing capture param, which is not a parameter of the method.
                let positional = match p.params.last().map(String::as_str) {
                    Some("__blockkw") => &p.params[..p.params.len() - 1],
                    _ => &p.params[..],
                };
                written_params(
                    positional,
                    p.splat,
                    p.arity.opt as usize,
                    &p.arity.kwnames,
                    &p.arity.kwreq,
                    p.arity.kwsplat.as_deref(),
                    p.arity.blockparam.as_deref(),
                )
            }
            Some(MethodShape::Builtin { arity, params, .. }) => {
                crate::arity_table::params_for(arity, params)
            }
            None => Vec::new(),
        }
    }
    /// `Proc#parameters`. Shaped like [`Self::method_parameters`], with one
    /// difference the reference is explicit about: a NON-lambda proc reports its
    /// required positionals as `:opt`, because a block accepts a call that does
    /// not supply them. The `lambda:` keyword forces either reading.
    ///
    /// ```console
    /// $ /opt/homebrew/opt/ruby/bin/ruby -e 'p proc { |a| }.parameters, lambda { |a| }.parameters'
    /// [[:opt, :a]]
    /// [[:req, :a]]
    /// ```
    pub fn proc_parameters(
        &self,
        v: &Value,
        lambda: Option<bool>,
    ) -> Vec<(&'static str, Option<String>)> {
        match self.obj(v) {
            Some(RObj::Proc {
                template,
                is_lambda,
                kind,
                ..
            }) => {
                // A curried or composed proc has no written parameter list left;
                // MRI describes all three as taking a bare rest.
                if matches!(
                    kind,
                    ProcKind::Curried { .. }
                        | ProcKind::MethodCurried { .. }
                        | ProcKind::Composed { .. }
                ) {
                    return vec![("rest", None)];
                }
                let p = &self.procs[*template];
                // The parser desugars a block's keyword params into one synthetic
                // trailing capture param, which is not a parameter of the proc.
                let positional = match p.params.last().map(String::as_str) {
                    Some("__blockkw") => &p.params[..p.params.len() - 1],
                    _ => &p.params[..],
                };
                let mut out = written_params(
                    positional,
                    p.splat,
                    p.arity.opt as usize,
                    &p.arity.kwnames,
                    &p.arity.kwreq,
                    p.arity.kwsplat.as_deref(),
                    p.arity.blockparam.as_deref(),
                );
                if !lambda.unwrap_or(*is_lambda) {
                    for e in out.iter_mut() {
                        if e.0 == "req" {
                            e.0 = "opt";
                        }
                    }
                }
                out
            }
            // `Symbol#to_proc` takes the receiver plus whatever the method takes.
            Some(RObj::SymProc(_)) => vec![("req", None), ("rest", None)],
            Some(RObj::Method {
                recv,
                name,
                unbound,
            }) => self.method_parameters(recv, name, *unbound),
            _ => Vec::new(),
        }
    }
    /// `Method#arity`. A written method reports the count of its required
    /// parameters, negated (`-(n+1)`) when the call shape is not a single fixed
    /// count; a built-in reports the arity the reference interpreter declares for
    /// it. `-1` is the last resort for a method nothing describes.
    pub fn method_arity(&self, recv: &Value, name: &str, unbound: bool) -> i64 {
        match self.resolve_method_shape(recv, name, unbound) {
            Some(MethodShape::Def { def, .. }) => ArityFacts::of_method(&def).arity_value(true),
            Some(MethodShape::Block { template, .. }) => {
                ArityFacts::of_proc(&self.procs[template]).arity_value(true)
            }
            Some(MethodShape::Builtin { arity, .. }) => arity as i64,
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
            Some(RObj::Proc { .. })
                | Some(RObj::SymProc(_))
                | Some(RObj::CycleProc(_))
                | Some(RObj::SeqProc(_))
                | Some(RObj::StepProc { .. })
                | Some(RObj::DeriveProc { .. })
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
            || self.find_singleton_class_method(cls, name).is_some()
            || self.find_class_alias(cls, name).is_some()
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
                || self.attr_access(&cls, name).is_some()
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
    /// How MRI names a receiver in a NoMethodError: `nil`/`true`/`false` as the
    /// literal, `class C`/`module M` for a class or module reference, and
    /// `an instance of C` for every other value. The one renderer for all of
    /// them, so a message raised from the numeric hook reads the same as one
    /// raised from method dispatch — several sites used to print the bare class
    /// name, which is the phrasing MRI uses for NOTHING.
    pub fn receiver_phrase(&self, v: &Value) -> String {
        match v {
            _ if is_main(v) => "main".to_string(),
            Value::Undef => "nil".to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            _ => match self.classref_name(v) {
                Some(c) => format!("{} {c}", self.class_or_module_word(&c)),
                None => format!("an instance of {}", self.class_of(v)),
            },
        }
    }
    /// The class name of any value — the dynamic class for a user object, the
    /// builtin class name otherwise.
    pub fn class_of(&self, v: &Value) -> String {
        if let Value::Obj(id) = v {
            if let Some(cls) = self.class_overrides.get(id) {
                return cls.clone();
            }
        }
        match self.obj(v) {
            Some(RObj::Object { class, .. }) => class.clone(),
            // A `module M` reference is an instance of `Module`, a `class C` one
            // an instance of `Class` — `Class < Module`, so only the module side
            // needs distinguishing.
            Some(RObj::ClassRef(n)) => if self.is_module_name(n) {
                "Module"
            } else {
                "Class"
            }
            .to_string(),
            _ => self.class_name(v).to_string(),
        }
    }
    /// The class used to *route* a value to its builtin method table: the raw
    /// native type for an override-backed builtin-subclass instance (so `Hash`
    /// ops still reach `dispatch_hash`), else `class_of`. Method resolution and
    /// `#class` use `class_of` (the override); only the native-op router uses this.
    pub fn dispatch_class(&self, v: &Value) -> String {
        if let Value::Obj(id) = v {
            if self.class_overrides.contains_key(id) {
                return self.class_name(v).to_string();
            }
        }
        self.class_of(v)
    }
    /// Follow the `alias_method`/`alias` chain for `name` on `class` (and its
    /// ancestors) to the underlying target name, when that target is *not* a
    /// bytecode method `find_method_owner` already resolves — i.e. a native
    /// builtin method (`class Params < Hash; alias_method :to_params_hash, :to_h`)
    /// or a reopened-builtin alias. Returns `None` when no alias applies.
    pub fn alias_target(&self, class: &str, name: &str) -> Option<String> {
        let anc = self.class_ancestry(class);
        let mut cur = name.to_string();
        let mut changed = false;
        for _ in 0..50 {
            let next = anc
                .iter()
                .find_map(|a| self.method_aliases.get(a).and_then(|m| m.get(&cur)));
            match next {
                Some(t) if *t != cur => {
                    cur = t.clone();
                    changed = true;
                }
                _ => break,
            }
        }
        // A snapshot alias of a native method resolves to that native method name.
        if let Some(native) = Self::native_alias_target(&cur) {
            return Some(native.to_string());
        }
        changed.then_some(cur)
    }
    /// Record that native value `v` is really an instance of user subclass
    /// `class` (a `X < Hash`/`Array`/`String`). See `class_overrides`.
    pub fn set_class_override(&mut self, v: &Value, class: &str) {
        if let Value::Obj(id) = v {
            self.class_overrides.insert(*id, class.to_string());
        }
    }
    /// If `class` is a *user* class whose superclass chain roots at a builtin
    /// collection, the builtin base (`"Hash"`/`"Array"`/`"String"`) whose native
    /// representation should back its instances; else `None`.
    pub fn builtin_container_root(&self, class: &str) -> Option<&'static str> {
        // Only user-defined classes need native backing; a bare `Hash`/`Array`
        // is already native.
        if !self.classes.contains_key(class) {
            return None;
        }
        let mut cur = Some(class.to_string());
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 100 {
                break;
            }
            match n.as_str() {
                "Hash" => return Some("Hash"),
                "Array" => return Some("Array"),
                "String" => return Some("String"),
                _ => {}
            }
            cur = self
                .classes
                .get(&n)
                .and_then(|d| d.superclass.clone())
                .map(|s| self.resolve_class_alias(&s, &n));
        }
        None
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
    /// The innermost `n` frame method names (deepest first) — debug aid.
    pub fn frame_method_tail(&self, n: usize) -> Vec<String> {
        self.frames
            .iter()
            .rev()
            .take(n)
            .filter_map(|f| f.scope.method_name.clone())
            .collect()
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
                None => break,
            }
        }
        // The implicit block parameter `it` (Ruby 3.4+) is bound under the
        // reserved name `__it__` (see the parser). Looking it up only after the
        // ordinary chain misses gives MRI's precedence: a real local named `it`
        // wins, so `it = 5; [1, 2].map { it }` is `[5, 5]` while
        // `[1, 2].map { it }` is `[1, 2]`.
        if name == "it" {
            return self.get_local("__it__");
        }
        Value::Undef
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
                None => break,
            }
        }
        // Same fallback as `get_local`: `it` also names the implicit block
        // parameter bound as `__it__`. Both lookups must agree, or a bare `it`
        // would be reported undefined and re-dispatched as a method call.
        if name == "it" {
            return self.local_defined("__it__");
        }
        false
    }
    pub fn get_global(&self, name: &str) -> Value {
        self.globals.get(name).cloned().unwrap_or(Value::Undef)
    }
    pub fn set_global(&mut self, name: &str, v: Value) {
        self.globals.insert(name.to_string(), v);
    }

    // ── output capture ───────────────────────────────────────────────────
    //
    // Every write a *program* makes to a native stream funnels through
    // `write_out`: `puts`/`print`/`p`/`printf`, `$stdout.write`, and ERB's
    // `run`. Diagnostics the runtime itself emits (the REPL banner, a crash
    // backtrace from `main`) deliberately do not — they belong to the process,
    // not to the program.

    /// Start capturing program output in-process. Any text already captured is
    /// discarded, so each run starts clean.
    pub fn begin_capture(&mut self) {
        self.capture = Some(String::new());
    }

    /// Stop capturing and take everything written since [`begin_capture`],
    /// returning the empty string when capture was not on.
    ///
    /// [`begin_capture`]: RubyHost::begin_capture
    pub fn end_capture(&mut self) -> String {
        self.capture.take().unwrap_or_default()
    }

    /// Whether output is being captured.
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Write program output: into the capture buffer when capturing, else to the
    /// native stream `stderr` selects. `s` is written verbatim — `puts` has
    /// already decided about the trailing newline.
    pub fn write_out(&mut self, s: &str, stderr: bool) {
        if let Some(buf) = &mut self.capture {
            buf.push_str(s);
            return;
        }
        use std::io::Write;
        if stderr {
            let mut o = std::io::stderr();
            let _ = o.write_all(s.as_bytes());
            let _ = o.flush();
        } else {
            let mut o = std::io::stdout();
            let _ = o.write_all(s.as_bytes());
            let _ = o.flush();
        }
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
            Value::Obj(id) => {
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
                    // A native-handle self (a Thread/Fiber method body): side table.
                    _ => self
                        .obj_ivars
                        .get(&id)
                        .and_then(|m| m.get(name))
                        .cloned()
                        .unwrap_or(Value::Undef),
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
                // A native-handle self (Thread/Fiber method body): side table.
                _ => {
                    self.obj_ivars
                        .entry(i)
                        .or_default()
                        .insert(name.to_string(), v);
                }
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
        // Walk the full ancestry (superclasses AND included/prepended modules):
        // ActionController mixes its `attr_internal` accessors (`view_runtime`) in
        // via modules, so a plain superclass-only walk would miss them.
        for c in self.class_ancestry(class) {
            if let Some((r, w)) = self.attr_accessors.get(&c).and_then(|m| m.get(field)) {
                if (writer && *w) || (!writer && *r) {
                    return Some((field.to_string(), writer));
                }
            }
            // An `alias_method` of a native attr accessor (ActiveSupport's
            // `attr_internal`): the alias name maps to the underlying field.
            if let Some((f, w)) = self.attr_aliases.get(&c).and_then(|m| m.get(method)) {
                return Some((f.clone(), *w));
            }
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
    /// Undo a `class_mixin`: drop `module` from the class's include/prepend/extend
    /// list. Used when re-routing a compile-time-registered include through a
    /// user `append_features` whose `super` re-adds it (ActiveSupport::Concern).
    pub fn remove_mixin(&mut self, class: &str, module: &str, kind: &str) {
        // Match by RESOLVED module name: the compile-time class-body extraction may
        // have stored an include unqualified ("Redirecting") because the module was
        // not yet registered when the class body compiled, while the runtime caller
        // passes the fully-qualified name ("ActionController::Redirecting"). Without
        // resolving, the entry is never dropped, so a Concern's `return false if
        // base < self` guard stays true and its dependencies are never included.
        let target = self.resolve_module_name(module, class);
        let list_names: Vec<String> = match self.classes.get(class) {
            Some(def) => match kind {
                "prepend" => def.prepends.clone(),
                "extend" => def.extends.clone(),
                _ => def.includes.clone(),
            },
            None => return,
        };
        let keep: Vec<bool> = list_names
            .iter()
            .map(|e| e != module && self.resolve_module_name(e, class) != target)
            .collect();
        if let Some(def) = self.classes.get_mut(class) {
            let list = match kind {
                "prepend" => &mut def.prepends,
                "extend" => &mut def.extends,
                _ => &mut def.includes,
            };
            let mut i = 0;
            list.retain(|_| {
                let k = keep[i];
                i += 1;
                k
            });
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
    /// Drop a class method (`def self.m`) so a later `singleton_class.define_method`
    /// redefinition wins over it — the two are the same singleton method in MRI,
    /// but rubylang stores them separately with the `def self.m` taking dispatch
    /// precedence (ActiveSupport's `redefine_singleton_method`).
    pub fn remove_class_method(&mut self, cls: &str, name: &str) {
        if let Some(def) = self.classes.get_mut(cls) {
            def.class_methods.shift_remove(name);
        }
    }
    /// Register an anonymous class/module (`Class.new`/`Module.new`) under a fresh
    /// name and return it. The optional superclass seeds the `ClassDef`; the block
    /// body (if any) is run afterwards as a `class_eval` by the caller.
    pub fn define_anon_class(&mut self, superclass: Option<String>, is_module: bool) -> String {
        self.struct_counter += 1;
        let kind = if is_module { "Module" } else { "Class" };
        let name = format!("#<{kind}:{}>", self.struct_counter);
        self.classes.insert(
            name.clone(),
            ClassDef {
                superclass,
                is_module,
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
    /// Whether the bundled stdlib `name` has already been run on this host, so a
    /// repeat `require` returns false without re-running it.
    pub fn embedded_stdlib_loaded(&self, name: &str) -> bool {
        self.embedded_stdlib_loaded.contains(name)
    }
    /// Record the bundled stdlib `name` as loaded. Called before its source runs,
    /// so a circular require inside it sees the library as already loaded.
    pub fn mark_embedded_stdlib_loaded(&mut self, name: &str) {
        self.embedded_stdlib_loaded.insert(name.to_string());
    }
    /// The class name a class variable read/write resolves against, given `self`:
    /// an instance's class, or a class-reference's own name.
    pub fn cvar_owner(&self, this: &Value) -> Option<String> {
        self.object_class(this).or_else(|| self.classref_name(this))
    }
    /// The class a `@@cvar` reference resolves against: the lexical class/module
    /// where the running code was defined (`def_class`), NOT the runtime receiver.
    /// A method mixed into another class via `extend`/`include` still reads its
    /// original module's class variables — ActionView's `register_template_handler`
    /// (defined in `Template::Handlers`, extended onto `Template`) mutates
    /// `Handlers`' `@@template_handlers`, not the extending class's. Falls back to
    /// the receiver's class for top-level code with no defining class.
    pub fn cvar_class(&self) -> Option<String> {
        let s = self.cur_scope();
        match &s.def_class {
            Some(dc) => Some(dc.clone()),
            None => self.cvar_owner(&s.self_obj),
        }
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
    /// Every singleton method name of `v`, sorted — `Object#singleton_methods`.
    /// A plain object contributes its `def obj.m` / `class << obj` methods plus
    /// any `define_singleton_method` block; a class or module contributes its
    /// class methods (`def self.m`), which ARE its singleton methods in MRI.
    pub fn singleton_method_names(&self, v: &Value) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        if let Value::Obj(id) = v {
            if let Some(m) = self.singleton_methods.get(id) {
                names.extend(m.keys().cloned());
            }
            if let Some(m) = self.singleton_define_methods.get(id) {
                names.extend(m.keys().cloned());
            }
        }
        if let Some(cls) = self.classref_name(v) {
            if let Some(def) = self.classes.get(&cls) {
                names.extend(def.class_methods.keys().cloned());
            }
            if let Some(m) = self.class_define_methods.get(&cls) {
                names.extend(m.keys().cloned());
            }
        }
        names.sort();
        names.dedup();
        names
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
    /// A `define_method` on the class's OWN singleton class `#<Class:class>`
    /// (`class.singleton_class.define_method`, `redefine_singleton_method`) — the
    /// class's own singleton method, which in MRI takes precedence over a class
    /// method inherited from an extended module's `ClassMethods`. Only the direct
    /// singleton class is consulted (not superclasses / includes), so it stays a
    /// same-level own-method check.
    pub fn own_singleton_define_method(&self, class: &str, name: &str) -> Option<Value> {
        let sclass = format!("#<Class:{class}>");
        self.define_methods
            .get(&sclass)
            .and_then(|m| m.get(name))
            .cloned()
    }
    /// A method defined on the singleton class of `class` or any of its
    /// superclasses (`Klass.singleton_class.class_eval { def m }` / `define_method`)
    /// — an *inherited* class method. Returns the owning singleton-class name (so
    /// the caller can invoke it), walking the object superclass chain because a
    /// singleton class's own superclass link is not modeled here.
    pub fn find_singleton_class_method(&self, class: &str, name: &str) -> Option<String> {
        let mut cur = Some(class.to_string());
        let mut guard = 0;
        while let Some(c) = cur {
            let sclass = format!("#<Class:{c}>");
            let has = self
                .define_methods
                .get(&sclass)
                .is_some_and(|m| m.contains_key(name))
                || self
                    .classes
                    .get(&sclass)
                    .is_some_and(|d| d.methods.contains_key(name));
            if has {
                return Some(sclass);
            }
            guard += 1;
            if guard > 100 {
                break;
            }
            cur = self.superclass_of(&c);
        }
        None
    }
    /// A `Klass.define_singleton_method` block for `name`, walking the superclass
    /// chain (a class-level singleton method is inherited like any class method).
    pub fn find_class_define_method(&self, class: &str, name: &str) -> Option<Value> {
        let mut cur = Some(class.to_string());
        let mut guard = 0;
        while let Some(c) = cur {
            // A `def self.m` via define_singleton_method / class-level define_method.
            if let Some(p) = self.class_define_methods.get(&c).and_then(|m| m.get(name)) {
                return Some(p.clone());
            }
            // `extend M` where M has an INSTANCE `define_method(:m)`: the module's
            // define-methods become the class's class methods. Rails'
            // AbstractController::Callbacks::ClassMethods defines before_action /
            // after_action / around_action this way (a `define_method` in a loop).
            if let Some(def) = self.classes.get(&c) {
                let extends = def.extends.clone();
                for module in extends.iter().rev() {
                    let resolved = self.resolve_module_name(module, &c);
                    for anc in self.module_self_ancestry(&resolved) {
                        if let Some(p) = self.define_methods.get(&anc).and_then(|m| m.get(name)) {
                            return Some(p.clone());
                        }
                    }
                }
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
    /// Whether `class` *itself* (not an ancestor) has a `define_method` for `name`.
    pub fn has_own_define_method(&self, class: &str, name: &str) -> bool {
        self.define_methods
            .get(class)
            .is_some_and(|m| m.contains_key(name))
    }
    /// Whether `class` *itself* (not an ancestor) defines the bytecode method `name`.
    pub fn has_own_method(&self, class: &str, name: &str) -> bool {
        self.classes
            .get(class)
            .is_some_and(|d| d.methods.contains_key(name))
    }
    /// A `define_method` block for `name`, walking the superclass chain.
    pub fn find_define_method(&self, class: &str, name: &str) -> Option<Value> {
        // Walk the full ancestry (included/prepended modules AND superclasses):
        // a `define_method` in a module body (`module M; define_method(:m){…}; end`,
        // as AbstractController::Callbacks generates `before_action`/`after_action`
        // in a loop) must be inherited by a class that includes M, not only found
        // on the class's own superclass chain.
        for c in self.class_ancestry(class) {
            if let Some(p) = self.define_methods.get(&c).and_then(|m| m.get(name)) {
                return Some(p.clone());
            }
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
    /// `alias`/`alias_method` with Ruby snapshot semantics: the alias captures the
    /// target method *as it is now*, so a later redefinition of the target does
    /// not change what the alias resolves to. A user (bytecode/`define_method`)
    /// target is copied under the alias name; a native builtin target is recorded
    /// with a `\x01native:` marker so the alias forwards to the native method even
    /// after a subclass overrides the target — `HashWithIndifferentAccess` aliases
    /// the native `[]=` to `regular_writer`, then overrides `[]=`; without the
    /// snapshot the override recurses into itself through the alias.
    /// Whether `method` is a native method of builtin `base` that a subclass
    /// commonly aliases to save before overriding. Not exhaustive — it only needs
    /// to cover the "alias the inherited native method, then redefine it" idiom.
    fn is_native_builtin_method(base: &str, method: &str) -> bool {
        match base {
            "Hash" => matches!(
                method,
                "[]" | "[]="
                    | "store"
                    | "fetch"
                    | "delete"
                    | "update"
                    | "merge"
                    | "merge!"
                    | "each"
                    | "each_pair"
                    | "keys"
                    | "values"
                    | "key?"
                    | "has_key?"
                    | "include?"
                    | "dig"
                    | "to_hash"
                    | "to_a"
                    | "size"
                    | "length"
                    | "default"
                    | "default="
                    | "default_proc"
                    | "default_proc="
                    | "replace"
                    | "clear"
                    | "assoc"
                    | "rassoc"
                    | "select"
                    | "reject"
                    | "invert"
                    | "key"
                    | "values_at"
                    | "slice"
            ),
            "Array" => matches!(
                method,
                "[]" | "[]="
                    | "push"
                    | "<<"
                    | "pop"
                    | "shift"
                    | "unshift"
                    | "each"
                    | "map"
                    | "size"
                    | "length"
                    | "first"
                    | "last"
                    | "to_a"
                    | "to_ary"
                    | "concat"
                    | "replace"
                    | "insert"
                    | "delete"
                    | "index"
                    | "include?"
            ),
            "String" => matches!(
                method,
                "[]" | "[]="
                    | "<<"
                    | "concat"
                    | "replace"
                    | "length"
                    | "size"
                    | "to_s"
                    | "to_str"
                    | "each_char"
                    | "gsub"
                    | "sub"
            ),
            _ => false,
        }
    }
    pub fn register_alias(&mut self, class: &str, alias_name: &str, target: &str) {
        // A builtin-backed subclass aliasing a native method of its base captures
        // the native method, even if the subclass also overrides it (rubylang
        // hoists the override into the method table, so a plain lookup would find
        // it and recurse). HashWithIndifferentAccess aliases native `[]=` to
        // `regular_writer`, then overrides `[]=` to call `regular_writer`.
        if let Some(base) = self.builtin_container_root(class) {
            if Self::is_native_builtin_method(base, target) {
                self.method_aliases
                    .entry(class.to_string())
                    .or_default()
                    .insert(alias_name.to_string(), format!("\u{1}native:{target}"));
                return;
            }
        }
        // A snapshot alias of a native Kernel function (`alias_method
        // :zeitwerk_original_require, :require`, taken *before* Zeitwerk's
        // `def require` in source order). This MUST precede the user-method
        // lookup: rubylang hoists a same-body `def require` into the method table
        // before the runtime `alias_method` runs, so a plain lookup would capture
        // the override — whose body calls the alias — and recurse forever. The
        // native marker forwards to the builtin instead.
        if crate::builtins::is_kernel_function(target) {
            self.method_aliases
                .entry(class.to_string())
                .or_default()
                .insert(alias_name.to_string(), format!("\u{1}native:{target}"));
            return;
        }
        if let Some((def, _)) = self.find_method_owner(class, target) {
            self.add_instance_method(class, alias_name, def);
            // Remember the original so `super` from the alias resolves as `target`
            // (the copied body may call `super`). Chase through an existing alias
            // so `alias a b; alias c a` records c's original as b's.
            let original = self
                .alias_originals
                .get(class)
                .and_then(|m| m.get(target))
                .cloned()
                .unwrap_or_else(|| target.to_string());
            self.alias_originals
                .entry(class.to_string())
                .or_default()
                .insert(alias_name.to_string(), original);
        } else if let Some(proc) = self.find_define_method(class, target) {
            self.define_methods
                .entry(class.to_string())
                .or_default()
                .insert(alias_name.to_string(), proc);
        } else if let Some((field, writer)) = self.attr_access(class, target) {
            // Aliasing a native attr accessor (ActiveSupport's `attr_internal`:
            // `alias_method :view_runtime=, :_view_runtime=`): the alias name reads
            // or writes the same underlying `@field`.
            self.attr_aliases
                .entry(class.to_string())
                .or_default()
                .insert(alias_name.to_string(), (field, writer));
        } else {
            // The target has no user definition — it is a native method (a class
            // method like `new`/`allocate`/`Time.at`, or an unresolved forward
            // reference). Snapshot it with the native marker so a *later*
            // redefinition of the target does not capture the alias (activesupport's
            // alias_method_chain: `alias_method :at_without_coercion, :at` before
            // `alias_method :at, :at_with_coercion` — a plain name alias would make
            // at_without_coercion resolve to at_with_coercion and recurse). The
            // native-marker dispatch invokes the builtin directly.
            self.method_aliases
                .entry(class.to_string())
                .or_default()
                .insert(alias_name.to_string(), format!("\u{1}native:{target}"));
        }
    }
    /// If a bareword/Kernel call `name` resolves (through the alias chain visible
    /// from `class`) to a snapshot alias of a native Kernel function, the native
    /// function name — so the caller can invoke the builtin directly, bypassing a
    /// later user redefinition of that name (Zeitwerk's `require` override).
    pub fn native_kernel_alias(&self, class: &str, name: &str) -> Option<String> {
        let anc = self.class_ancestry(class);
        let mut cur = name.to_string();
        for _ in 0..50 {
            let next = anc
                .iter()
                .find_map(|a| self.method_aliases.get(a).and_then(|m| m.get(&cur)));
            match next {
                Some(t) if *t != cur => cur = t.clone(),
                _ => break,
            }
        }
        Self::native_alias_target(&cur)
            .filter(|n| crate::builtins::is_kernel_function(n))
            .map(|n| n.to_string())
    }
    /// If `target` is a `\x01native:<name>` marker (a snapshot alias of a native
    /// method), the underlying native method name; else `None`.
    pub fn native_alias_target(target: &str) -> Option<&str> {
        target.strip_prefix("\u{1}native:")
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
    /// A *class-method* alias for `name` — one registered on the singleton class
    /// of `class` or any of its superclasses (`class << self; alias new! new`).
    /// Walks the object superclass chain (singleton classes are not linked here),
    /// so a subclass inherits an alias defined on an ancestor's singleton.
    pub fn find_class_alias(&self, class: &str, name: &str) -> Option<String> {
        let mut cur = Some(class.to_string());
        let mut guard = 0;
        while let Some(c) = cur {
            let sclass = format!("#<Class:{c}>");
            if let Some(t) = self.method_aliases.get(&sclass).and_then(|m| m.get(name)) {
                return Some(t.clone());
            }
            guard += 1;
            if guard > 100 {
                break;
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
        if let Some(v) = self.attr_accessors.shift_remove(old) {
            self.attr_accessors.insert(new.to_string(), v);
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
    /// Flag `class` as having run a bare `module_function` (its instance methods
    /// double as module methods).
    pub fn mark_module_function(&mut self, class: &str) {
        self.module_function_modules.insert(class.to_string());
    }
    /// Whether `class` (or an ancestor) ran a bare `module_function`.
    pub fn is_module_function_module(&self, class: &str) -> bool {
        self.module_function_modules.contains(class)
    }
    /// Whether the constant `name` names a module rather than a class — either a
    /// built-in one (`Comparable`, `Enumerable`, …) or one the program opened
    /// with `module`. Drives `Module#class`, `is_a?`/`instance_of?`, and the
    /// singleton lookup chain, all of which differ between the two.
    pub fn is_module_name(&self, name: &str) -> bool {
        // `Module` and `Class` themselves are *classes* (`Module.class` is
        // `Class` in MRI), so only the table's module list and the program's own
        // `module` openings count.
        crate::arity_table::is_module(name) || self.classes.get(name).is_some_and(|d| d.is_module)
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
        // short name the constant points to is itself fully qualified. Try the
        // name as-is, then qualified against `from`'s enclosing namespaces: a
        // `class Concurrent::Map < Collection::MapImplementation` names the const
        // `Concurrent::Collection::MapImplementation`, not a bare one.
        let mut const_candidates = vec![name.to_string()];
        let mut cprefix = from;
        while let Some(idx) = cprefix.rfind("::") {
            cprefix = &cprefix[..idx];
            const_candidates.push(format!("{cprefix}::{name}"));
        }
        for cand in &const_candidates {
            let c = self.get_const(cand);
            if let Some(RObj::ClassRef(real)) = self.obj(&c) {
                if real != name && real != from {
                    return self.resolve_class_alias(real, from);
                }
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
            cur = def
                .superclass
                .clone()
                .map(|s| self.resolve_class_alias(&s, &name));
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
        // Walk the ancestry of the value's class when that class is user-defined
        // — covers both a plain user object and a native-backed builtin subclass
        // (`class Params < Hash`), whose `class_of` is the override, so
        // `params.is_a?(Hash)` and `is_a?(Enumerable)` hold.
        if self.classes.contains_key(&actual) && self.class_is_ancestor(&actual, class) {
            return true;
        }
        // Builtin exceptions (and user subclasses of them) only place correctly
        // through the full ancestry: `class_is_ancestor` stops at the first name
        // with no class-table entry, which is exactly where the builtin part of
        // the tree begins (`MyErr < ArgumentError < StandardError`).
        if is_builtin_exception_name(&actual)
            || self.classes.contains_key(&actual)
            // A `Struct.new` / `Data.define` class need not be in the class table
            // at all — it registers as a struct definition — so neither gate above
            // sees it, and `Trio.new(1, 2).is_a?(Struct)` was false while
            // `Trio.ancestors` already listed `Struct`.
            || self.struct_defs.contains_key(&actual)
        {
            return self.class_ancestry(&actual).iter().any(|a| a == class);
        }
        false
    }
    /// The ancestor chain of a class (self first), including modules, matching
    /// `Module#ancestors`. Builtin types use a fixed table; user classes walk
    /// their superclass chain and included modules, then close with the
    /// `Object`/`Kernel`/`BasicObject` root.
    /// Resolve a `prepend`/`include`/`extend` module reference recorded in a
    /// class body to its registered class-table name. A bare `Helpers` written
    /// inside `Rack::Request` names the nested `Rack::Request::Helpers`, which
    /// only registers at runtime; so try the enclosing namespace first, then the
    /// name as written, then a class alias. Falls back to the given name so the
    /// ancestor chain still lists it (matching Ruby, which shows the module even
    /// when unresolved here).
    pub fn resolve_module_name(&self, module: &str, enclosing: &str) -> String {
        let nested = format!("{enclosing}::{module}");
        if self.classes.contains_key(&nested) {
            return nested;
        }
        // Walk outward through the enclosing namespace: inside `module ActiveSupport`
        // a `module Callbacks` doing `extend Concern` names the sibling
        // `ActiveSupport::Concern`, not `ActiveSupport::Callbacks::Concern` or a
        // top-level `Concern`. Mirrors lexical constant resolution.
        let mut prefix = enclosing;
        while let Some(idx) = prefix.rfind("::") {
            prefix = &prefix[..idx];
            let cand = format!("{prefix}::{module}");
            if self.classes.contains_key(&cand) {
                return cand;
            }
        }
        if self.classes.contains_key(module) {
            return module.to_string();
        }
        let alias = self.resolve_class_alias(module, enclosing);
        if self.classes.contains_key(&alias) {
            return alias;
        }
        module.to_string()
    }
    /// The tail every ancestor chain ends in — the common root, preceded by the
    /// generating class when `name` is a `Struct.new` / `Data.define` class. MRI
    /// keeps that class in the chain, and it is where the generated surface is
    /// defined: `Struct` mixes in `Enumerable` (so a struct is `each`-able and
    /// answers `map`/`select`/…), while `Data` deliberately does not.
    fn ancestry_tail(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.struct_defs.contains_key(name) {
            out.push(
                if self.is_data_class(name) {
                    "Data"
                } else {
                    "Struct"
                }
                .to_string(),
            );
            if !self.is_data_class(name) {
                out.push("Enumerable".to_string());
            }
        }
        out.extend(["Object", "Kernel", "BasicObject"].map(String::from));
        out
    }
    pub fn class_ancestry(&self, name: &str) -> Vec<String> {
        let own = |mods: &[&str]| {
            let mut v = vec![name.to_string()];
            v.extend(mods.iter().map(|s| s.to_string()));
            v.extend(self.ancestry_tail(name));
            v
        };
        match name {
            "BasicObject" => vec!["BasicObject".into()],
            "Object" => vec!["Object".into(), "Kernel".into(), "BasicObject".into()],
            // Bare modules are their own only ancestor here.
            "Kernel" | "Comparable" | "Enumerable" => vec![name.into()],
            // `Class < Module` in MRI, so `Class.superclass` is `Module` and a
            // Module method is callable on a Class.
            "Class" => own(&["Module"]),
            "Numeric" => own(&["Comparable"]),
            "Integer" | "Float" | "Rational" => own(&["Numeric", "Comparable"]),
            "Complex" => own(&["Numeric"]),
            "String" | "Symbol" | "Time" | "Date" => own(&["Comparable"]),
            "DateTime" => own(&["Date", "Comparable"]),
            "Array" | "Hash" | "Range" | "Set" | "Struct" => own(&["Enumerable"]),
            // A user-defined `module M`: prepends, itself, then its includes.
            // A module has no superclass, so the `Object`/`Kernel`/`BasicObject`
            // tail a class carries must not appear — `module B; include A; end`
            // gives `[B, A]` in MRI, not `[B, A, Object, Kernel, BasicObject]`.
            _ if self.classes.get(name).is_some_and(|d| d.is_module) => {
                let def = &self.classes[name];
                let mut out: Vec<String> = def
                    .prepends
                    .iter()
                    .rev()
                    .map(|m| self.resolve_module_name(m, name))
                    .collect();
                out.push(name.to_string());
                for m in def.includes.iter().rev() {
                    let inc = self.resolve_module_name(m, name);
                    // A module's own includes contribute their chains too.
                    out.extend(self.class_ancestry(&inc));
                }
                dedup_keep_first(out)
            }
            _ => {
                if self.classes.contains_key(name) {
                    // A user-defined class: self, its included modules, then up
                    // the superclass chain; finally the common root.
                    let mut out = Vec::new();
                    let mut cur = Some(name.to_string());
                    while let Some(n) = cur {
                        // Prepended modules precede the class in the chain.
                        if let Some(def) = self.classes.get(&n) {
                            let mods: Vec<String> = def.prepends.clone();
                            for m in mods.iter().rev() {
                                out.push(self.resolve_module_name(m, &n));
                            }
                        }
                        out.push(n.clone());
                        match self.classes.get(&n) {
                            Some(def) => {
                                let mods: Vec<String> = def.includes.clone();
                                for m in mods.iter().rev() {
                                    out.push(self.resolve_module_name(m, &n));
                                }
                                cur = def
                                    .superclass
                                    .clone()
                                    .map(|s| self.resolve_class_alias(&s, &n));
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
                    out.extend(self.ancestry_tail(name));
                    dedup_keep_first(out)
                } else if is_builtin_exception_name(name) {
                    // Walk the real MRI exception tree, so `rescue StandardError`
                    // and `#is_a?` agree with it: most errors sit under
                    // StandardError, and several have their own intermediate
                    // parent (`NoMethodError < NameError < StandardError`).
                    let mut v = vec![name.to_string()];
                    let mut cur = name;
                    while let Some(parent) = builtin_exception_parent(cur) {
                        v.push(parent.to_string());
                        cur = parent;
                    }
                    v.extend(["Object", "Kernel", "BasicObject"].map(String::from));
                    v
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
    /// Immediate subclasses of `class` — every registered class whose direct
    /// superclass is `class` (`Class#subclasses`, Ruby 3.1+). Anonymous helper
    /// classes (`#<Class:N>`) are excluded, matching how Rails filters them.
    pub fn direct_subclasses(&self, class: &str) -> Vec<String> {
        self.classes
            .keys()
            .filter(|k| !k.starts_with("#<"))
            .filter(|k| self.class_superclass(k).as_deref() == Some(class))
            .cloned()
            .collect()
    }
    /// All transitive descendants of `class` (`ActiveSupport`'s `descendants`),
    /// breadth-first over `direct_subclasses`.
    pub fn all_descendants(&self, class: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut queue = self.direct_subclasses(class);
        while let Some(c) = queue.pop() {
            if out.contains(&c) {
                continue;
            }
            queue.extend(self.direct_subclasses(&c));
            out.push(c);
        }
        out
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
        // Every builtin exception is a class constant too, including the few not
        // spelled `*Error` (`SystemExit`, `SignalException`, `Interrupt`). They
        // are all top-level, so a qualified name is never one of them — without
        // that guard a namespaced probe like `Foo::NegativeError` would resolve
        // to a phantom class just for ending in "Error".
        if is_builtin_exception_name(name) && (!name.contains("::") || name.starts_with("Errno::"))
        {
            return true;
        }
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
                | "UnboundMethod"
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
                // The `Errno` namespace itself, so `Errno::ENOENT` resolves as a
                // constant path rather than dispatching `ENOENT` on nil.
                | "Errno"
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
            let rp = self.resolve_module_name(p, &name);
            if let Some(r) = self.find_in_module(&rp, method) {
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
            let ri = self.resolve_module_name(i, &name);
            if let Some(r) = self.find_in_module(&ri, method) {
                return Some(r);
            }
        }
        None
    }
    /// The builtin superclass of a reopened builtin class that carries no
    /// recorded `superclass` — so a method added to a reopened `Numeric` is found
    /// from an `Integer` receiver. activesupport reopens `Numeric` with
    /// `days`/`hours`/… that `Integer`/`Float` must inherit.
    fn builtin_superclass(name: &str) -> Option<String> {
        match name {
            "Integer" | "Float" | "Rational" | "Complex" => Some("Numeric".to_string()),
            _ => None,
        }
    }
    /// The builtin modules a builtin type includes — so a method added to a
    /// reopened `Enumerable`/`Comparable` is found from an `Array`/`Integer`
    /// receiver. activesupport reopens `Enumerable` with `index_by`/`pluck`/… and
    /// `Comparable`; every collection/number must inherit them.
    fn builtin_modules(name: &str) -> &'static [&'static str] {
        match name {
            "Array" | "Hash" | "Range" | "Set" | "Struct" | "Enumerator" => &["Enumerable"],
            "Integer" | "Float" | "Rational" | "Numeric" | "String" | "Symbol" | "Time"
            | "Date" | "DateTime" => &["Comparable"],
            // Object includes Kernel, so a reopened `module Kernel` (activesupport's
            // core-ext: silence_warnings, suppress, …) resolves for every object
            // and for bareword self-calls at the top level (self = main : Object).
            "Object" => &["Kernel"],
            _ => &[],
        }
    }
    pub fn find_method_owner(&self, class: &str, method: &str) -> Option<(MethodDef, String)> {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            if let Some(def) = self.classes.get(&name) {
                // Prepended modules sit ahead of the class's own methods (last
                // prepend wins, matching Ruby's reverse-order ancestor insertion).
                for module in def.prepends.iter().rev() {
                    let m = self.resolve_module_name(module, &name);
                    if let Some(r) = self.find_in_module(&m, method) {
                        return Some(r);
                    }
                }
                if let Some(m) = def.methods.get(method) {
                    return Some((m.clone(), name.clone()));
                }
                // An `alias`/`alias_method` on this class resolves to its target
                // method (activesupport's `alias :cattr_accessor :mattr_accessor`
                // on `Module`). Resolve the target from this class onward.
                if let Some(target) = self.method_aliases.get(&name).and_then(|m| m.get(method)) {
                    let target = target.clone();
                    if let Some(found) = self.find_method_owner(&name, &target) {
                        return Some(found);
                    }
                }
                // Included modules (transitively) take priority over the
                // superclass (last include wins). Resolve each include against
                // *this* class's namespace so a bare `include Helpers` inside
                // `Rack::Request` finds `Rack::Request::Helpers`, not an unrelated
                // `Sinatra::Helpers` a suffix match picks.
                for module in def.includes.iter().rev() {
                    let m = self.resolve_module_name(module, &name);
                    if let Some(r) = self.find_in_module(&m, method) {
                        return Some(r);
                    }
                }
            }
            // Builtin included modules (Enumerable/Comparable) — checked even when
            // the class itself was not reopened, since the *module* may have been.
            for module_name in Self::builtin_modules(&name) {
                if self.classes.contains_key(*module_name) {
                    if let Some(r) = self.find_in_module(module_name, method) {
                        return Some(r);
                    }
                }
            }
            cur = self
                .classes
                .get(&name)
                .and_then(|d| d.superclass.clone())
                .map(|s| self.resolve_class_alias(&s, &name))
                .or_else(|| Self::builtin_superclass(&name))
                .or_else(|| {
                    // A user class with no explicit superclass inherits from Object
                    // (whose builtin_modules include Kernel), so continue the walk
                    // there — a reopened `module Kernel` method (silence_warnings)
                    // must resolve for instances of any user class, not just main.
                    if self.classes.contains_key(&name) && name != "Object" && name != "BasicObject"
                    {
                        Some("Object".to_string())
                    } else {
                        None
                    }
                });
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
    /// Ruby makes a handful of hook methods private by definition, so they never
    /// appear in `instance_methods`/`public_methods` however they were declared.
    const PRIVATE_HOOKS: &[&str] = &[
        "initialize",
        "initialize_copy",
        "initialize_clone",
        "initialize_dup",
        "respond_to_missing?",
        "method_missing",
    ];

    /// `Module#instance_methods` — the public AND protected names, which is what
    /// MRI returns (it excludes only private).
    pub fn instance_method_names(&self, class: &str, inherited: bool) -> Vec<String> {
        self.instance_method_names_vis(
            class,
            inherited,
            &[Visibility::Public, Visibility::Protected],
        )
    }

    /// The instance-method names of `class` whose visibility is one of `want` —
    /// the shared body behind `instance_methods` / `public_instance_methods` /
    /// `private_instance_methods` / `protected_instance_methods`.
    ///
    /// `PRIVATE_HOOKS` are private by definition in MRI however they were
    /// declared, so they answer to the private query and to no other.
    pub fn instance_method_names_vis(
        &self,
        class: &str,
        inherited: bool,
        want: &[Visibility],
    ) -> Vec<String> {
        let chain: Vec<String> = if inherited {
            self.class_ancestry(class)
        } else {
            vec![class.to_string()]
        };
        let mut out = Vec::new();
        for n in &chain {
            let vis_of = |k: &str| {
                if Self::PRIVATE_HOOKS.contains(&k) {
                    Visibility::Private
                } else {
                    self.own_visibility(n, k)
                }
            };
            let take = |keys: Box<dyn Iterator<Item = &String> + '_>, out: &mut Vec<String>| {
                for k in keys {
                    if !k.starts_with("__") && want.contains(&vis_of(k)) {
                        out.push(k.clone());
                    }
                }
            };
            if let Some(def) = self.classes.get(n) {
                take(Box::new(def.methods.keys()), &mut out);
            }
            if let Some(dm) = self.define_methods.get(n) {
                take(Box::new(dm.keys()), &mut out);
            }
        }
        dedup_keep_first(out)
    }

    /// The visibility `class` records for its OWN entry `method`, without
    /// consulting ancestors. Public when nothing is recorded.
    pub fn own_visibility(&self, class: &str, method: &str) -> Visibility {
        self.classes
            .get(class)
            .and_then(|d| d.visibility.get(method))
            .copied()
            .unwrap_or_default()
    }

    /// The effective visibility of `method` as seen through `class`: the entry
    /// recorded by the first ancestor that has an opinion, so a subclass can make
    /// an inherited method private (`private :to_s`) and a public redefinition
    /// further down wins over a private one further up.
    pub fn method_visibility(&self, class: &str, method: &str) -> Visibility {
        if Self::PRIVATE_HOOKS.contains(&method) {
            return Visibility::Private;
        }
        for n in self.class_ancestry(class) {
            if let Some(def) = self.classes.get(&n) {
                if let Some(v) = def.visibility.get(method) {
                    return *v;
                }
                // An ancestor that *defines* the method without recording a
                // visibility defines it public, and that definition shadows any
                // private entry further up the chain.
                if def.methods.contains_key(method) {
                    return Visibility::Public;
                }
            }
            if self
                .define_methods
                .get(&n)
                .is_some_and(|dm| dm.contains_key(method))
            {
                return Visibility::Public;
            }
        }
        Visibility::Public
    }

    /// Record `vis` for `class`'s entry `method` (`private :m`, `public :m`, and
    /// the class-body modifier the compiler resolves).
    pub fn set_method_visibility(&mut self, class: &str, method: &str, vis: Visibility) {
        let def = self.classes.entry(class.to_string()).or_default();
        if vis == Visibility::Public {
            def.visibility.shift_remove(method);
        } else {
            def.visibility.insert(method.to_string(), vis);
        }
    }

    /// Record `vis` for `class`'s CLASS method `method` —
    /// `private_class_method :m` / `public_class_method :m`.
    pub fn set_class_method_visibility(&mut self, class: &str, method: &str, vis: Visibility) {
        let def = self.classes.entry(class.to_string()).or_default();
        if vis == Visibility::Public {
            def.class_visibility.shift_remove(method);
        } else {
            def.class_visibility.insert(method.to_string(), vis);
        }
    }

    /// The visibility of `class`'s class method `method`.
    ///
    /// Walks the superclass chain because a class method is inherited and so is
    /// the restriction on it: `private_class_method :new` on a base class hides
    /// `new` on every subclass. `extend`ed and `include`d modules are not walked
    /// — `private_class_method` records against the class it is called on, and a
    /// module's own instance-method visibility is a different map.
    pub fn class_method_visibility(&self, class: &str, method: &str) -> Visibility {
        let mut cur = Some(class.to_string());
        while let Some(name) = cur {
            let Some(def) = self.classes.get(&name) else {
                break;
            };
            if let Some(v) = def.class_visibility.get(method) {
                return *v;
            }
            // A subclass that redefines the class method defines it public, and
            // that shadows a private entry further up the chain.
            if def.class_methods.contains_key(method) {
                return Visibility::Public;
            }
            cur = def.superclass.clone();
        }
        Visibility::Public
    }
    /// Whether `method` is defined on `class` or any ancestor (own methods,
    /// included modules, superclasses, and `define_method`-created methods),
    /// regardless of visibility — the `*_method_defined?` builtins narrow the
    /// answer with [`RubyHost::method_visibility`] on top.
    pub fn is_method_defined(&self, class: &str, method: &str) -> bool {
        self.find_method_owner(class, method).is_some()
            || self.find_define_method(class, method).is_some()
    }
    /// Resolve a `super` call: find `method` in the receiver's linearized
    /// ancestry *after* the position of `def_class` (the current method's
    /// owner). Walking the receiver's full ancestry — not just `def_class`'s
    /// superclass — is what makes `prepend`/`include` super reach the class
    /// method that follows in `Module#ancestors` order.
    /// The original method name an alias was created from, for `super` resolution
    /// (walking `class` and its ancestors, since the alias may live on an ancestor).
    pub fn alias_original(&self, class: &str, alias_name: &str) -> Option<String> {
        for anc in self.class_ancestry(class) {
            if let Some(orig) = self
                .alias_originals
                .get(&anc)
                .and_then(|m| m.get(alias_name))
            {
                return Some(orig.clone());
            }
        }
        None
    }
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
                // Resolve the extended module lexically (nested, then outward
                // through the enclosing namespace) then through its own aliases
                // and includes. The owner is the MODULE the method actually lives
                // in (not the extending class) so `def_class` is correct — a `super`
                // resumes above the module, and a `@@cvar` reference resolves to the
                // module's class variables (ActionView's `register_template_handler`
                // extended onto `Template` still mutates `Handlers`' cvars).
                let resolved = self.resolve_module_name(module, &name);
                if let Some((m, owner)) = self.find_in_module(&resolved, method) {
                    return Some((m, owner));
                }
            }
            cur = def
                .superclass
                .clone()
                .map(|s| self.resolve_class_alias(&s, &name));
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
                let resolved = self.resolve_module_name(module, &name);
                if let Some((m, _)) = self.find_in_module(&resolved, method) {
                    return Some(m);
                }
            }
            cur = def
                .superclass
                .clone()
                .map(|s| self.resolve_class_alias(&s, &name));
        }
        None
    }
    /// A module's own ancestry (the module, its prepends, and its includes,
    /// recursively) in resolution order. Used to linearize an extended module's
    /// contribution to a class's singleton (class-method) ancestry.
    fn module_self_ancestry(&self, module: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_module_ancestry(module, &mut out, 0);
        out
    }
    fn collect_module_ancestry(&self, module: &str, out: &mut Vec<String>, depth: usize) {
        if depth > 50 || out.iter().any(|m| m == module) {
            return;
        }
        if let Some(def) = self.classes.get(module) {
            let prepends = def.prepends.clone();
            for p in prepends.iter().rev() {
                let rp = self.resolve_module_name(p, module);
                self.collect_module_ancestry(&rp, out, depth + 1);
            }
            out.push(module.to_string());
            let includes = def.includes.clone();
            for i in includes.iter().rev() {
                let ri = self.resolve_module_name(i, module);
                self.collect_module_ancestry(&ri, out, depth + 1);
            }
        } else {
            out.push(module.to_string());
        }
    }
    /// `super` from a singleton/class method: resume class-method lookup in the
    /// receiver's singleton (class-method) ancestry *after* the currently-running
    /// method's owner (`def_class`). `recv_class` is the actual receiver class, so
    /// the walk spans its whole superclass chain — a `def self.m` (`class << self`)
    /// AND an extended-module class method (`extend ClassMethods`) both resume
    /// correctly. `def_class` may be one of the extended modules (its owner), which
    /// has no superclass of its own, so a plain `superclass_of(def_class)` walk
    /// would miss the rest of the chain.
    pub fn find_super_class_method(
        &self,
        recv_class: &str,
        def_class: &str,
        method: &str,
    ) -> Option<(MethodDef, String)> {
        // Linearize the class-method ancestry: for each class up the superclass
        // chain, its own `def self.m` methods, then its extended modules (last
        // extend first) each expanded through their own include/prepend ancestry,
        // mirroring `find_class_method_owner`'s order. Expanding the module
        // ancestry is what lets `def_class` (which may be a module nested inside an
        // extended module) be found in the list.
        let mut sources: Vec<(String, bool)> = Vec::new(); // (name, is_extend_module)
        let mut cur = Some(recv_class.to_string());
        while let Some(name) = cur {
            let Some(def) = self.classes.get(&name) else {
                break;
            };
            sources.push((name.clone(), false));
            for module in def.extends.iter().rev() {
                let resolved = self.resolve_module_name(module, &name);
                for anc in self.module_self_ancestry(&resolved) {
                    sources.push((anc, true));
                }
            }
            cur = def
                .superclass
                .clone()
                .map(|s| self.resolve_class_alias(&s, &name));
        }
        // Dedup by name (keep first occurrence): a module reachable via more than
        // one path must appear once, else `super` could resume at a later copy of
        // the *same* owner and re-invoke the running method forever.
        {
            let mut seen = std::collections::HashSet::new();
            sources.retain(|(n, _)| seen.insert(n.clone()));
        }
        // Resume just after `def_class` (the running method's owner). If it is not
        // in the linearized ancestry, there is no super — returning None here (vs.
        // restarting at the top) avoids re-selecting the same method and recursing.
        let pos = sources.iter().position(|(n, _)| n == def_class)?;
        let start = pos + 1;
        for (name, is_module) in sources.iter().skip(start) {
            if *is_module {
                // An extended module's instance method becomes a class method;
                // `find_in_module` follows the module's own include/prepend chain.
                if let Some((m, owner)) = self.find_in_module(name, method) {
                    return Some((m, owner));
                }
            } else if let Some(def) = self.classes.get(name) {
                if let Some(m) = def.class_methods.get(method) {
                    return Some((m.clone(), name.clone()));
                }
            }
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
            // Native-handle objects (Thread/Fiber/…): read from the side table.
            _ => match obj {
                Value::Obj(i) => self
                    .obj_ivars
                    .get(i)
                    .and_then(|m| m.get(name))
                    .cloned()
                    .unwrap_or(Value::Undef),
                _ => Value::Undef,
            },
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
            match self.heap.get_mut(*i as usize) {
                Some(RObj::Object { ivars, .. }) => {
                    ivars.insert(name.to_string(), v);
                }
                // Native-handle objects (Thread/Fiber/IO/…) store ivars in the
                // side table keyed by heap id.
                _ => {
                    self.obj_ivars
                        .entry(*i)
                        .or_default()
                        .insert(name.to_string(), v);
                }
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
    pub fn super_context(&mut self) -> (Value, Option<String>, Option<String>, Vec<Value>) {
        let s = self.cur_scope();
        let self_obj = s.self_obj.clone();
        let method_name = s.method_name.clone();
        let def_class = s.def_class.clone();
        (self_obj, method_name, def_class, self.zsuper_args())
    }
    /// The arguments a bare `super` (no parens) forwards: the CURRENT values of
    /// the enclosing method's formal parameters — positional (splat expanded)
    /// plus a rebuilt trailing keyword hash — read from the live locals. This
    /// matches MRI, where `super` re-passes the parameters as they stand now
    /// (including default values and any reassignment inside the method), not the
    /// raw arguments the method was originally called with. Falls back to the
    /// frame's original args when the method def can't be resolved.
    fn zsuper_args(&mut self) -> Vec<Value> {
        let s = self.cur_scope();
        let (Some(method), Some(def_class)) = (s.method_name.clone(), s.def_class.clone()) else {
            return self
                .frames
                .last()
                .map(|f| f.args.clone())
                .unwrap_or_default();
        };
        let Some(def) = self.find_method(&def_class, &method) else {
            return self
                .frames
                .last()
                .map(|f| f.args.clone())
                .unwrap_or_default();
        };
        let mut out = Vec::new();
        for (i, p) in def.params.iter().enumerate() {
            let v = self.get_local(p);
            if def.splat == Some(i) {
                // A `*rest` parameter holds an array — expand it inline.
                match self.as_array(&v) {
                    Some(items) => out.extend(items),
                    None => out.push(v),
                }
            } else {
                out.push(v);
            }
        }
        // Rebuild the trailing keyword hash from `name:` params and any `**opts`
        // collector, so `super` forwards keyword arguments too.
        if !def.kwparams.is_empty() || def.kwsplat.is_some() {
            let mut map: IndexMap<RKey, Value> = IndexMap::new();
            for kw in &def.kwparams {
                let v = self.get_local(kw);
                map.insert(RKey::Sym(kw.clone()), v);
            }
            if let Some(ks) = &def.kwsplat {
                let hv = self.get_local(ks);
                if let Some(h) = self.as_hash(&hv) {
                    for (k, val) in h {
                        map.insert(k, val);
                    }
                }
            }
            if !map.is_empty() {
                let hash = self.new_hash(map);
                out.push(hash);
            }
        }
        out
    }

    // ---- exceptions -------------------------------------------------------

    pub fn set_pending_exc(&mut self, v: Value) {
        self.pending_exc = Some(v);
    }
    pub fn take_pending_exc(&mut self) -> Option<Value> {
        self.pending_exc.take()
    }
    /// Whether a raised exception is currently pending (an `Err` in flight is a
    /// real raise, not a soft "bareword isn't a method" signal).
    pub fn has_pending_exc(&self) -> bool {
        self.pending_exc.is_some()
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
            n.ends_with("Error")
                || n == "Exception"
                || n == "StopIteration"
                || n.starts_with("Errno::")
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
        if exc_class == rescued || rescued == "Exception" {
            return true;
        }
        // A name we have no record of cannot be placed in the tree — many
        // builtins report a failure by message alone. Treat it as a plain
        // StandardError so a bare `rescue` still fires.
        if !self.classes.contains_key(exc_class) && !is_builtin_exception_name(exc_class) {
            return rescued == "StandardError";
        }
        self.class_ancestry(exc_class).iter().any(|a| a == rescued)
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

    /// The `(heap id, elision marker)` MRI would use if rendering `v` re-entered
    /// `v` itself. `None` for everything that cannot contain itself — the
    /// overwhelmingly common case, and one heap lookup.
    fn cycle_marker(&self, v: &Value) -> Option<(u32, String)> {
        let Value::Obj(id) = v else { return None };
        let marker = match self.obj(v)? {
            RObj::Array(_) => "[...]".to_string(),
            RObj::Hash { .. } => "{...}".to_string(),
            RObj::Set(_) => "Set[...]".to_string(),
            RObj::Object { class, .. } if self.struct_defs.contains_key(class) => format!(
                "#<{} {class}:...>",
                if self.is_data_class(class) {
                    "data"
                } else {
                    "struct"
                }
            ),
            _ => return None,
        };
        Some((*id, marker))
    }

    /// `to_s` — the human string form used by `puts`/interpolation. Renders the
    /// MRI elision marker instead of recursing when a container holds itself.
    pub fn to_s(&mut self, v: &Value) -> String {
        let Some((id, marker)) = self.cycle_marker(v) else {
            return self.uncycled_to_s(v);
        };
        if self.rendering.contains(&id) {
            return marker;
        }
        self.rendering.push(id);
        let out = self.uncycled_to_s(v);
        self.rendering.pop();
        out
    }

    fn uncycled_to_s(&mut self, v: &Value) -> String {
        if is_main(v) {
            return "main".to_string();
        }
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
                // MRI nests one `#<Enumerator::Lazy: …>` per pipeline stage
                // around the object `.lazy` was called on, tagged with the
                // operation that added it.
                Some(RObj::Lazy { ops, origin, .. }) => {
                    let mut s = format!("#<Enumerator::Lazy: {}>", self.inspect(&origin));
                    for op in &ops {
                        s = format!("#<Enumerator::Lazy: {s}:{}>", self.lazy_op_tag(op));
                    }
                    s
                }
                Some(RObj::Enumerator {
                    buf,
                    method,
                    source,
                    ..
                }) => {
                    // MRI shows `#<Enumerator: <receiver>:<method>>`. The receiver
                    // is the recorded source when there is one; otherwise the
                    // materialized values stand in for it (they match for the
                    // common Array case).
                    let recv = match source {
                        Some(s) => self.inspect(&s),
                        None => self.inspect_array(&buf),
                    };
                    format!("#<Enumerator: {recv}:{method}>")
                }
                Some(RObj::Generator { .. }) => {
                    "#<Enumerator: #<Enumerator::Generator>:each>".to_string()
                }
                Some(RObj::Yielder { .. }) | Some(RObj::FiberYielder) => {
                    "#<Enumerator::Yielder>".to_string()
                }
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
                Some(RObj::ObjRange { lo, hi, exclusive }) => {
                    format!(
                        "{}{}{}",
                        self.inspect(&lo),
                        if exclusive { "..." } else { ".." },
                        self.inspect(&hi)
                    )
                }
                Some(RObj::Array(items)) => self.inspect_array(&items),
                Some(RObj::Hash { map, .. }) => self.inspect_hash(&map),
                Some(RObj::Proc { .. })
                | Some(RObj::SymProc(_))
                | Some(RObj::CycleProc(_))
                | Some(RObj::SeqProc(_))
                | Some(RObj::StepProc { .. })
                | Some(RObj::DeriveProc { .. }) => "#<Proc>".to_string(),
                // MRI renders the DEFINING module, not the receiver's class, and
                // tags an UnboundMethod as one. (MRI also appends the written
                // parameter list and source location; rubylang retains neither.)
                Some(RObj::Method {
                    recv,
                    name,
                    unbound,
                }) => {
                    let tag = if unbound { "UnboundMethod" } else { "Method" };
                    format!(
                        "#<{tag}: {}#{name}>",
                        self.method_owner(&recv, &name, unbound)
                    )
                }
                Some(RObj::Regexp { source, flags, .. }) => {
                    let on: String = "mix".chars().filter(|c| flags.contains(*c)).collect();
                    let off: String = "mix".chars().filter(|c| !flags.contains(*c)).collect();
                    format!("(?{on}-{off}:{source})")
                }
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

    /// `inspect` — the debug form used by `p`/`inspect` (quotes strings). Shares
    /// `rendering` with `to_s`, so a struct whose member is the struct itself
    /// elides once no matter which of the two entered first.
    pub fn inspect(&mut self, v: &Value) -> String {
        let Some((id, marker)) = self.cycle_marker(v) else {
            return self.inspect_uncycled(v);
        };
        if self.rendering.contains(&id) {
            return marker;
        }
        self.rendering.push(id);
        let out = self.inspect_uncycled(v);
        self.rendering.pop();
        out
    }

    fn inspect_uncycled(&mut self, v: &Value) -> String {
        if is_main(v) {
            return "main".to_string();
        }
        match v {
            Value::Undef => "nil".to_string(),
            Value::Str(s) => inspect_string(s),
            Value::Obj(_) => match self.obj(v).cloned() {
                Some(RObj::Str(s)) => inspect_string(&s),
                Some(RObj::Symbol(s)) => {
                    if plain_symbol_name(&s) {
                        format!(":{s}")
                    } else {
                        format!(":{}", inspect_string(&s))
                    }
                }
                Some(RObj::BigInt(b)) => b.to_string(),
                Some(RObj::Rational(r)) => format!("({}/{})", r.numer(), r.denom()),
                // MRI's `Complex#inspect` inspects each part, so a Rational part
                // is parenthesized (`(11/25)`), and an imaginary part that does
                // not end in a digit takes an explicit `*i`.
                Some(RObj::Complex { re, im }) => {
                    let re_s = self.inspect(&re);
                    // The sign comes from the VALUE, not its rendering: a
                    // negative Rational inspects as `(-1/8)`, whose leading
                    // character is a paren.
                    let negative = self.as_f64(&im).is_some_and(|f| f < 0.0);
                    let mag_v = if negative {
                        self.num_op(NumOp::Sub, &Value::Int(0), &im)
                            .unwrap_or_else(|_| im.clone())
                    } else {
                        im.clone()
                    };
                    let (sign, mag) = (if negative { "-" } else { "+" }, self.inspect(&mag_v));
                    let unit = if mag.ends_with(|c: char| c.is_ascii_digit()) {
                        "i"
                    } else {
                        "*i"
                    };
                    format!("({re_s}{sign}{mag}{unit})")
                }
                Some(RObj::Set(map)) => {
                    let inner: Vec<String> = map.values().map(|v| self.inspect(v)).collect();
                    format!("Set[{}]", inner.join(", "))
                }
                Some(RObj::Array(items)) => self.inspect_array(&items),
                Some(RObj::Hash { map, .. }) => self.inspect_hash(&map),
                Some(RObj::Regexp { source, flags, .. }) => {
                    let f: String = "mix".chars().filter(|c| flags.contains(*c)).collect();
                    format!("/{source}/{f}")
                }
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
                // `Exception#inspect` is NOT the message — MRI wraps it with the
                // class, which is what makes `p e` in a rescue body readable:
                //
                //   $ /opt/homebrew/opt/ruby/bin/ruby -e 'p RuntimeError.new("x")'
                //   #<RuntimeError: x>
                //   $ /opt/homebrew/opt/ruby/bin/ruby -e 'p ArgumentError.new("")'
                //   ArgumentError
                //   $ /opt/homebrew/opt/ruby/bin/ruby -e 'p RuntimeError.new("a\nb")'
                //   #<RuntimeError:"a\nb">
                //
                // An empty message degrades to the bare class name, and a
                // multi-line one is inspected (and loses the space) so the form
                // stays on one line.
                Some(RObj::Object { class, ivars }) if self.is_exception_class(&class) => {
                    let msg = match ivars.get("message") {
                        Some(m) => self.to_s(&m.clone()),
                        None => class.clone(),
                    };
                    if msg.is_empty() {
                        class
                    } else if msg.contains('\n') {
                        format!("#<{class}:{}>", inspect_string(&msg))
                    } else {
                        format!("#<{class}: {msg}>")
                    }
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
                _ => self.uncycled_to_s(v),
            },
            _ => self.uncycled_to_s(v),
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
                    RKey::Sym(s) if plain_symbol_name(s) => format!("{s}: {vs}"),
                    // A symbol key that needs quoting keeps the `key:` shorthand
                    // but quotes the name: `{"a b": 1}`, as MRI does.
                    RKey::Sym(s) => format!("{}: {vs}", inspect_string(s)),
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
            RKey::Sym(s) if plain_symbol_name(s) => format!(":{s}"),
            RKey::Sym(s) => format!(":{}", inspect_string(s)),
            RKey::Bool(b) => b.to_string(),
            RKey::Nil => "nil".to_string(),
            RKey::FloatBits(b) => fmt_float(f64::from_bits(*b)),
            RKey::Class(n) => n.clone(),
            RKey::Array(ks) => {
                let parts: Vec<String> = ks.clone().iter().map(|k| self.key_inspect(k)).collect();
                format!("[{}]", parts.join(", "))
            }
            // `mix` order, matching `Regexp#inspect`.
            RKey::Regexp(source, bits) => {
                let flags: String = [(4, 'm'), (1, 'i'), (2, 'x')]
                    .iter()
                    .filter(|(b, _)| bits & b != 0)
                    .map(|(_, c)| *c)
                    .collect();
                format!("/{source}/{flags}")
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
            RKey::Identity(i) => self.inspect(&Value::Obj(*i)),
            // Hash/Set/BigInt/Rational/Complex keys have no shorthand rendering
            // of their own — rebuild the value and use the one `inspect`.
            k @ (RKey::Hash(_)
            | RKey::Set(_)
            | RKey::Big(_)
            | RKey::Rational(_)
            | RKey::Complex(..)) => {
                let v = self.key_to_value(&k.clone());
                self.inspect(&v)
            }
            // Only reachable through a container that keys itself, which the
            // renderer elides the same way.
            RKey::Recursive => "[...]".to_string(),
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
                Some(RObj::Yielder { .. }) | Some(RObj::FiberYielder) => "Enumerator::Yielder",
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
                Some(RObj::ObjRange { .. }) => "Range",
                Some(RObj::Proc { .. })
                | Some(RObj::SymProc(_))
                | Some(RObj::CycleProc(_))
                | Some(RObj::SeqProc(_))
                | Some(RObj::StepProc { .. })
                | Some(RObj::DeriveProc { .. }) => "Proc",
                // An UnboundMethod is its own class in MRI, with its own surface
                // (`bind`/`bind_call` but no `call`/`receiver`), so it must not
                // report itself as a `Method`.
                Some(RObj::Method { unbound: true, .. }) => "UnboundMethod",
                Some(RObj::Method { .. }) => "Method",
                Some(RObj::Regexp { .. }) => "Regexp",
                Some(RObj::MatchData { .. }) => "MatchData",
                Some(RObj::ClassRef(n)) => {
                    if self.is_module_name(n) {
                        "Module"
                    } else {
                        "Class"
                    }
                }
                Some(RObj::Object { .. }) => "Object",
                None => "Object",
            },
            _ => "Object",
        }
    }

    /// `Object#hash`: a stable integer hash consistent with `eql?`/`==` (equal
    /// values hash equally), derived from the value's canonical `RKey` — the same
    /// key Hash uses. So `"a".hash == "a".hash` and `[1,2].hash == [1,2].hash`,
    /// which gems' own hash-keyed caches (mustermann's `EqualityMap`) rely on.
    pub fn value_hash(&self, v: &Value) -> i64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.to_key(v).hash(&mut hasher);
        hasher.finish() as i64
    }
    fn to_key(&self, v: &Value) -> RKey {
        self.to_key_seen(v, &mut Vec::new())
    }

    /// `to_key`, carrying the stack of container heap ids already being keyed.
    /// A container that holds itself would otherwise recurse until the native
    /// stack overflows; MRI answers a finite `hash` for one, so the re-entry
    /// keys as [`RKey::Recursive`] and the walk terminates.
    fn to_key_seen(&self, v: &Value, seen: &mut Vec<u32>) -> RKey {
        if let Value::Obj(id) = v {
            if seen.contains(id) {
                return RKey::Recursive;
            }
            if matches!(
                self.obj(v),
                Some(RObj::Array(_) | RObj::Hash { .. } | RObj::Set(_) | RObj::Object { .. })
            ) {
                seen.push(*id);
                let k = self.to_key_inner(v, seen);
                seen.pop();
                return k;
            }
        }
        self.to_key_inner(v, seen)
    }

    fn to_key_inner(&self, v: &Value, seen: &mut Vec<u32>) -> RKey {
        match v {
            Value::Int(n) => RKey::Int(*n),
            // `0.0.eql?(-0.0)` is true in Ruby and the two are the SAME Hash key,
            // but their bit patterns differ — normalize the sign of zero away.
            Value::Float(f) => RKey::FloatBits(if *f == 0.0 {
                0f64.to_bits()
            } else {
                f.to_bits()
            }),
            Value::Bool(b) => RKey::Bool(*b),
            Value::Undef => RKey::Nil,
            Value::Str(s) => RKey::Str(s.to_string()),
            Value::Obj(_) => match self.obj(v) {
                Some(RObj::Str(s)) => RKey::Str(s.clone()),
                Some(RObj::Symbol(s)) => RKey::Sym(s.clone()),
                Some(RObj::ClassRef(n)) => RKey::Class(n.clone()),
                Some(RObj::Array(items)) => {
                    RKey::Array(items.iter().map(|e| self.to_key_seen(e, seen)).collect())
                }
                // A Hash and a Set are unordered for equality and hashing
                // (`{a: 1, b: 2}` equals `{b: 2, a: 1}` and hashes the same), so
                // both sort their entries into a canonical order before keying.
                Some(RObj::Hash { map, .. }) => {
                    let mut pairs: Vec<(RKey, RKey)> = map
                        .iter()
                        .map(|(k, v)| (k.clone(), self.to_key_seen(v, seen)))
                        .collect();
                    pairs.sort();
                    RKey::Hash(pairs)
                }
                Some(RObj::Set(items)) => {
                    let mut ks: Vec<RKey> = items.keys().cloned().collect();
                    ks.sort();
                    RKey::Set(ks)
                }
                // The non-`i64` numerics key by VALUE like every other number, so
                // `h[2**64]` and `h[Rational(1, 2)]` find their entry back. Each
                // keeps its own class, so `1`, `2**64`, `Rational(1, 1)` and
                // `Complex(1, 0)` remain four distinct keys as in Ruby.
                Some(RObj::BigInt(b)) => RKey::Big(b.clone()),
                Some(RObj::Rational(q)) => RKey::Rational(q.clone()),
                Some(RObj::Complex { re, im }) => {
                    let (re, im) = (re.clone(), im.clone());
                    RKey::Complex(
                        Box::new(self.to_key_seen(&re, seen)),
                        Box::new(self.to_key_seen(&im, seen)),
                    )
                }
                Some(RObj::Range { lo, hi, exclusive }) => RKey::Range(*lo, *hi, *exclusive),
                Some(RObj::StrRange { lo, hi, exclusive }) => {
                    RKey::StrRange(lo.clone(), hi.clone(), *exclusive)
                }
                Some(RObj::FloatRange { lo, hi, exclusive }) => {
                    RKey::FloatRange(lo.to_bits(), hi.to_bits(), *exclusive)
                }
                // A Struct/Data instance compares and hashes BY VALUE in Ruby —
                // two `P.new(1, 2)` are the same hash key and report the same
                // `hash` — so key it on its class plus its members.
                Some(RObj::Object { class, ivars }) if self.struct_defs.contains_key(class) => {
                    let mut parts = vec![RKey::Class(class.clone())];
                    parts.extend(ivars.values().map(|m| self.to_key_seen(m, seen)));
                    RKey::Array(parts)
                }
                // Keyed by the same `(source, options)` pair `eq_values`
                // compares, so `hash`/`eql?` stay consistent with `==`.
                Some(RObj::Regexp { source, flags, .. }) => {
                    RKey::Regexp(source.clone(), regex_option_bits(flags))
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
            RKey::Hash(pairs) => {
                let map: IndexMap<RKey, Value> = pairs
                    .clone()
                    .iter()
                    .map(|(k, v)| (k.clone(), self.key_to_value(v)))
                    .collect();
                self.new_hash(map)
            }
            RKey::Set(ks) => {
                let items: Vec<Value> = ks.clone().iter().map(|k| self.key_to_value(k)).collect();
                self.new_set(items)
            }
            RKey::Big(b) => self.new_bigint(b.clone()),
            RKey::Rational(q) => self.new_rational(q.clone()),
            RKey::Complex(re, im) => {
                let (re, im) = (self.key_to_value(re), self.key_to_value(im));
                self.new_complex(re, im)
            }
            RKey::Range(lo, hi, excl) => self.new_range(*lo, *hi, *excl),
            RKey::StrRange(lo, hi, excl) => self.new_str_range(lo.clone(), hi.clone(), *excl),
            RKey::FloatRange(lo, hi, excl) => {
                self.new_float_range(f64::from_bits(*lo), f64::from_bits(*hi), *excl)
            }
            // Rebuilt with the flag letters in Ruby's canonical `mix` order, the
            // same order `to_s`/`inspect` render, so a Regexp taken back out of a
            // Hash key inspects exactly as the one put in.
            RKey::Regexp(source, bits) => {
                let flags: String = [(4, 'm'), (1, 'i'), (2, 'x')]
                    .iter()
                    .filter(|(b, _)| bits & b != 0)
                    .map(|(_, c)| *c)
                    .collect();
                // The source compiled once already; if the engine somehow
                // rejects it now there is no Regexp to answer with.
                self.new_regex(source, &flags).unwrap_or(Value::Undef)
            }
            RKey::Identity(i) => Value::Obj(*i),
            // A recursive container's key has no standalone value to rebuild;
            // the container it stands for is reachable from the outer key.
            RKey::Recursive => Value::Undef,
        }
    }

    /// MRI's `rb_cmperr`: the `ArgumentError` a `Comparable` operator raises
    /// when `<=>` answers nil.
    ///
    /// The operand is NAMED by `inspect` when it is a Float or a special
    /// constant — so a NaN reads `NaN` and `nil`/`:sym` read as themselves —
    /// and by its CLASS otherwise (`String`, `Array`, `Object`). MRI switches on
    /// `SPECIAL_CONST_P(y) || RB_FLOAT_TYPE_P(y)`; the equivalent here is a
    /// non-heap `Value` plus Symbol, which is a heap object here but a special
    /// constant there.
    pub fn cmp_failed(&mut self, recv: &Value, other: &Value) -> String {
        let by_inspect = match other {
            Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Undef => true,
            _ => matches!(self.obj(other), Some(RObj::Symbol(_))),
        };
        let rhs = if by_inspect {
            self.inspect(other)
        } else {
            self.class_of(other)
        };
        crate::builtins::raise_exc(
            "ArgumentError",
            &format!("comparison of {} with {rhs} failed", self.class_of(recv)),
        )
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
            // `-2i` / `-Complex(1, 2)` — negate both parts.
            if let Some((re, im)) = self.complex_parts(a) {
                let nr = self.num_op(Sub, &Value::Int(0), &re)?;
                let ni = self.num_op(Sub, &Value::Int(0), &im)?;
                return Ok(self.new_complex(nr, ni));
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
                // MRI names the receiver `an instance of C` (or the bare
                // literal for nil/true/false), never by its class alone.
                _ => Err(format!(
                    "undefined method '-@' for {}",
                    self.receiver_phrase(a)
                )),
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
        // Rational takes `< <= > >=` from Comparable, so they are DERIVED from
        // `Rational#<=>`: when that answers nil the operator raises rather than
        // answering false. The two cases where it answers nil are a NaN operand
        // and an operand with no rational value at all.
        //
        // Only when the RECEIVER is the Rational. `Float::NAN < Rational(1, 2)`
        // is `Float#<`, which is plain IEEE and answers false without ever
        // consulting `<=>`, so the check cannot be written as "either side is a
        // NaN" -- it is asymmetric, exactly as MRI is.
        if matches!(op, Lt | Gt | Le | Ge) && matches!(self.obj(a), Some(RObj::Rational(_))) {
            let comparable = match b {
                Value::Float(f) => !f.is_nan(),
                Value::Int(_) => true,
                _ => self.as_rational(b).is_some(),
            };
            if !comparable {
                return Err(self.cmp_failed(a, b));
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
        // Float arithmetic. The VM computes Float ops inline and never routes
        // them here, so this path exists for the recursive COMPONENT operations
        // a Complex/Rational with a Float part makes (`Complex(1.5, 2.5) * 2`
        // multiplies `1.5` by `2` through `num_op`).
        let complex_side = matches!(self.obj(a), Some(RObj::Complex { .. }))
            || matches!(self.obj(b), Some(RObj::Complex { .. }));
        if !complex_side && (matches!(a, Value::Float(_)) || matches!(b, Value::Float(_))) {
            // ORDERING against a Float is exact in Ruby, so it must not go
            // through the `as_f64` promotion the arithmetic below uses: rounding
            // `10**52` to a double lands it exactly on `(10**52).to_f` and
            // reports `10**52 <= (10**52).to_f` as true, where MRI says false.
            if matches!(op, Lt | Gt | Le | Ge) {
                if let Some(ord) = self.exact_num_cmp(a, b) {
                    return Ok(Value::Bool(match op {
                        Lt => ord.is_lt(),
                        Gt => ord.is_gt(),
                        Le => ord.is_le(),
                        _ => ord.is_ge(),
                    }));
                }
            }
            if let (Some(x), Some(y)) = (self.as_f64(a), self.as_f64(b)) {
                let out = match op {
                    Add => Some(Value::Float(x + y)),
                    Sub => Some(Value::Float(x - y)),
                    Mul => Some(Value::Float(x * y)),
                    Div => Some(Value::Float(x / y)),
                    Mod => Some(Value::Float(x - y * (x / y).floor())),
                    Pow => Some(Value::Float(x.powf(y))),
                    Lt => Some(Value::Bool(x < y)),
                    Gt => Some(Value::Bool(x > y)),
                    Le => Some(Value::Bool(x <= y)),
                    Ge => Some(Value::Bool(x >= y)),
                    _ => None,
                };
                if let Some(v) = out {
                    return Ok(v);
                }
            }
        }
        // Complex arithmetic: `(a+bi) op (c+di)`, promoting a real operand to
        // `(real, 0)`. Component operations recurse through `num_op` so the parts
        // keep their own numeric types.
        if complex_side {
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
            // `String + x` demands a String. `to_s`-ing the operand made
            // `"a" + 1` answer `"a1"` where Ruby raises a TypeError.
            (Some(RObj::Str(s)), Add) => {
                let s = s.clone();
                return match self.as_str(b) {
                    Some(bs) => Ok(self.new_string(format!("{s}{bs}"))),
                    None => Err(self.no_conversion(b, "String")),
                };
            }
            (Some(RObj::Str(s)), Mul) => {
                let s = s.clone();
                // MRI REFUSES a negative repeat count rather than treating it as
                // zero: `"x" * -1` is `ArgumentError: negative argument`.
                let raw = self.to_int_operand(b)?;
                if raw < 0 {
                    return Err(crate::builtins::raise_exc(
                        "ArgumentError",
                        "negative argument",
                    ));
                }
                let n = raw as usize;
                return Ok(self.new_string(s.repeat(n)));
            }
            (Some(RObj::Str(s)), Lt | Gt | Le | Ge) => {
                if let Some(RObj::Str(bs)) = self.obj(b) {
                    return Ok(Value::Bool(cmp_ord(op, s.cmp(bs))));
                }
            }
            (Some(RObj::Array(mut xs)), Add) => {
                let Some(RObj::Array(ys)) = self.obj(b).cloned() else {
                    return Err(self.no_conversion(b, "Array"));
                };
                xs.extend(ys);
                return Ok(self.new_array(xs));
            }
            // `Array - Array`: difference, preserving order and duplicates in the
            // left operand that are absent from the right. This is the NATIVE
            // arrival of `-`; `Array#difference` reaches the same operation
            // through method dispatch, and both must use `eql?` membership.
            (Some(RObj::Array(xs)), Sub) => {
                if let Some(RObj::Array(ys)) = self.obj(b).cloned() {
                    let kept: Vec<Value> = xs
                        .iter()
                        .filter(|v| !ys.iter().any(|w| self.eql_values(v, w)))
                        .cloned()
                        .collect();
                    return Ok(self.new_array(kept));
                }
            }
            // `Array * String` is `join`; `Array * Integer` repeats. Anything
            // else converts with `to_int` and so is a TypeError.
            (Some(RObj::Array(xs)), Mul) => {
                if let Some(sep) = self.as_str(b) {
                    let parts: Vec<String> = xs.iter().map(|v| self.to_s(v)).collect();
                    return Ok(self.new_string(parts.join(&sep)));
                }
                // MRI REFUSES a negative repeat count rather than treating it as
                // zero: `"x" * -1` is `ArgumentError: negative argument`.
                let raw = self.to_int_operand(b)?;
                if raw < 0 {
                    return Err(crate::builtins::raise_exc(
                        "ArgumentError",
                        "negative argument",
                    ));
                }
                let n = raw as usize;
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
                let in_ys = |v: &Value, this: &Self| ys.iter().any(|w| this.eql_values(v, w));
                let result: Vec<Value> = match op {
                    Add => xs.iter().chain(ys.iter()).cloned().collect(),
                    _ => xs.iter().filter(|v| !in_ys(v, self)).cloned().collect(),
                };
                return Ok(self.new_set(result));
            }
        }
        // A NUMERIC receiver reaching here means the operand could not be
        // coerced, which Ruby reports as a TypeError naming both sides — not as
        // a missing method on a class that plainly has `+`.
        if matches!(op, Add | Sub | Mul | Div | Mod | Pow) {
            let recv_class = self.class_of(a);
            if matches!(
                recv_class.as_str(),
                "Integer" | "Float" | "Rational" | "Complex"
            ) {
                return Err(format!(
                    "{} can't be coerced into {recv_class}",
                    self.coerce_operand_name(b)
                ));
            }
        }
        // An ORDERING operator on a Comparable receiver is not a missing method:
        // `<` is defined, it asked `<=>`, and `<=>` answered nil. MRI reports
        // that as `comparison of Integer with String failed`. Only the
        // arithmetic half of this fallthrough had its own diagnostic, so `1 + "a"`
        // named both sides while `1 < "a"` claimed Integer has no `<`.
        if matches!(op, Lt | Gt | Le | Ge) {
            let cls = self.class_of(a);
            let comparable = crate::arity_table::ancestry(&cls)
                .is_some_and(|anc| anc.contains(&"Comparable"))
                || self
                    .expanded_ancestry(&cls)
                    .iter()
                    .any(|c| c == "Comparable");
            if comparable {
                return Err(self.cmp_failed(a, b));
            }
        }
        // Same phrasing as method dispatch: an arithmetic operator that the
        // receiver does not have is an ordinary NoMethodError.
        Err(format!(
            "undefined method '{}' for {}",
            num_op_name(op),
            self.receiver_phrase(a)
        ))
    }

    /// How MRI names an operand inside a coercion TypeError: `nil`, `true` and
    /// `false` by literal, a Symbol by its `:name` form, anything else by class.
    pub fn coerce_operand_name(&self, v: &Value) -> String {
        match v {
            Value::Undef => "nil".to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            _ => match self.obj(v) {
                Some(RObj::Symbol(s)) => format!(":{s}"),
                _ => self.class_of(v),
            },
        }
    }

    /// MRI's `no implicit conversion of X into <target>`, for an operand that
    /// had to be of a particular class. `nil`/`true`/`false` are named by
    /// literal; everything else by class.
    pub fn no_conversion(&self, v: &Value, target: &str) -> String {
        let named = match v {
            Value::Undef => "nil".to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            _ => self.class_of(v),
        };
        format!("no implicit conversion of {named} into {target}")
    }

    /// MRI's implicit integer conversion for a NATIVE operator operand. A Float
    /// truncates; `nil` and everything else raise rather than silently reading
    /// as 0, which had made `"ab" * nil` answer `""`.
    pub fn to_int_operand(&self, v: &Value) -> Result<i64, String> {
        match v {
            Value::Int(n) => Ok(*n),
            Value::Float(f) => Ok(*f as i64),
            Value::Undef => Err("no implicit conversion from nil to integer".to_string()),
            _ => as_int(v).ok_or_else(|| self.no_conversion(v, "Integer")),
        }
    }

    /// Structural equality (`==`).
    /// Ruby's OTHER equality. `==` coerces across the numeric classes
    /// (`1 == 1.0`, `1 == 1r`) while `eql?` — the half of the `hash`/`eql?` pair
    /// that Hash keys, `uniq`, the Array set operators and `Set` are all defined
    /// in terms of — does not. The rule is RECURSIVE: a container is `eql?` to
    /// another only when their corresponding elements are, so `[1].eql?([1.0])`
    /// is false even though `[1] == [1.0]`, and likewise one and two levels
    /// deeper. Anything that is neither a number nor a container falls through
    /// to `==`, which is what Ruby's default `eql?` amounts to for it.
    pub fn eql_values(&self, a: &Value, b: &Value) -> bool {
        // Same cycle guard `eq_values` carries, for the same reason: `eql?` is
        // the recursive half of the `hash`/`eql?` pair, so `uniq`, the Array set
        // operators and `Set` all reach it, and a self-referential container
        // walked without a guard aborts the process.
        if let (Value::Obj(x), Value::Obj(y)) = (a, b) {
            if self.is_container(a) && self.is_container(b) {
                let pair = (*x, *y);
                if EQL_PAIRS.with(|p| p.borrow().contains(&pair)) {
                    return true;
                }
                EQL_PAIRS.with(|p| p.borrow_mut().push(pair));
                let out = self.eql_values_uncycled(a, b);
                EQL_PAIRS.with(|p| {
                    p.borrow_mut().pop();
                });
                return out;
            }
        }
        self.eql_values_uncycled(a, b)
    }

    fn eql_values_uncycled(&self, a: &Value, b: &Value) -> bool {
        let numeric = |c: &str| matches!(c, "Integer" | "Float" | "Rational" | "Complex");
        let (ca, cb) = (self.class_of(a), self.class_of(b));
        if ca != cb && numeric(&ca) && numeric(&cb) {
            return false;
        }
        match (self.obj(a), self.obj(b)) {
            (Some(RObj::Array(x)), Some(RObj::Array(y))) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.eql_values(p, q))
            }
            // A Hash's KEYS are already `eql?`-strict — they are `RKey`s, where an
            // Integer and a Float never share a variant — so only the values need
            // the recursion.
            (Some(RObj::Hash { map: x, .. }), Some(RObj::Hash { map: y, .. })) => {
                x.len() == y.len()
                    && x.iter()
                        .all(|(k, v)| y.get(k).is_some_and(|w| self.eql_values(v, w)))
            }
            (Some(RObj::Complex { re: xr, im: xi }), Some(RObj::Complex { re: yr, im: yi })) => {
                self.eql_values(xr, yr) && self.eql_values(xi, yi)
            }
            // A Struct/Data instance is `eql?` when it is the same class and
            // every member is `eql?` — `S.new(1)` and `S.new(1.0)` are not.
            (
                Some(RObj::Object {
                    class: sa,
                    ivars: ia,
                }),
                Some(RObj::Object {
                    class: sb,
                    ivars: ib,
                }),
            ) if self.struct_defs.contains_key(sa) => {
                sa == sb
                    && ia.len() == ib.len()
                    && ia
                        .iter()
                        .all(|(k, v)| ib.get(k).is_some_and(|w| self.eql_values(v, w)))
            }
            _ => self.eq_values(a, b),
        }
    }

    /// A Float's EXACT integer value, or `None` when it has none — it is NaN,
    /// infinite, or carries a fractional part. This is what makes Ruby's
    /// `Integer == Float` exact rather than a lossy promotion of the Integer:
    /// `2**64 == 2.0**64` is true because `2.0**64` is exactly `2**64`, while
    /// `(2**64 + 1) == 2.0**64` is false and `(10**23) == 1e23` is false.
    fn exact_float_int(v: &Value) -> Option<num_bigint::BigInt> {
        use num_traits::FromPrimitive as _;
        let f = match v {
            Value::Float(f) => *f,
            _ => return None,
        };
        if !f.is_finite() || f.fract() != 0.0 {
            return None;
        }
        num_bigint::BigInt::from_f64(f)
    }

    /// Order two numbers EXACTLY, or `None` when one of them is not a finite
    /// number (NaN/infinity, or not a number at all) and so has no exact
    /// rational value to compare. Every finite double IS a rational, so a mixed
    /// Integer/Float pair never has to round either side.
    pub fn exact_num_cmp(&self, a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
        let exact = |v: &Value| match v {
            Value::Float(f) if f.is_finite() => num_rational::BigRational::from_float(*f),
            Value::Float(_) => None,
            _ => self.as_rational(v),
        };
        Some(exact(a)?.cmp(&exact(b)?))
    }

    /// MRI's `rb_equal`, which is what every collection SEARCH uses — not `==`.
    ///
    /// `rb_equal` answers true immediately when the two operands are the same
    /// VALUE, and only then asks `==`. The two differ on exactly one value: a
    /// NaN is not `==` itself, but it IS itself, so MRI answers
    ///
    ///   $ /opt/homebrew/opt/ruby/bin/ruby -e 'p [Float::NAN].include?(Float::NAN)'
    ///   true
    ///
    /// while `Float::NAN == Float::NAN` stays false. Identity here is bit
    /// equality on the `Value`, which is what MRI's flonum comparison amounts to;
    /// it can only ever turn a false into a true, since anything `==` already
    /// accepts is unaffected.
    pub fn rb_equal(&self, a: &Value, b: &Value) -> bool {
        if let (Value::Float(x), Value::Float(y)) = (a, b) {
            if x.to_bits() == y.to_bits() {
                return true;
            }
        }
        self.eq_values(a, b)
    }

    pub fn eq_values(&self, a: &Value, b: &Value) -> bool {
        // Container equality is a structural walk, so a container that holds
        // itself would recurse until the native stack overflows. MRI's
        // `rb_exec_recursive_paired` answers TRUE for a pair it is already
        // deciding, which is why
        //
        //   $ /opt/homebrew/opt/ruby/bin/ruby -e 'a=[1];a<<a;b=[1];b<<b;p a==b'
        //   true
        //
        // The guard is only armed for containers: everything else is compared
        // by value in one step and cannot re-enter.
        if let (Value::Obj(x), Value::Obj(y)) = (a, b) {
            if self.is_container(a) && self.is_container(b) {
                let pair = (*x, *y);
                if EQ_PAIRS.with(|p| p.borrow().contains(&pair)) {
                    return true;
                }
                EQ_PAIRS.with(|p| p.borrow_mut().push(pair));
                let out = self.eq_values_uncycled(a, b);
                EQ_PAIRS.with(|p| {
                    p.borrow_mut().pop();
                });
                return out;
            }
        }
        self.eq_values_uncycled(a, b)
    }

    /// Whether `v` is one of the containers whose equality walk descends into
    /// elements, and so can reach itself. A PLAIN object is excluded: its
    /// equality is one step (or a user `==`), it cannot cycle, and arming the
    /// guard for it only risks a false positive.
    fn is_container(&self, v: &Value) -> bool {
        match self.obj(v) {
            Some(RObj::Array(_) | RObj::Hash { .. } | RObj::Set(_)) => true,
            Some(RObj::Object { class, .. }) => self.struct_defs.contains_key(class),
            _ => false,
        }
    }

    fn eq_values_uncycled(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            // `Integer == Float` is EXACT in Ruby, at every Integer width. Below
            // 2**53 an `as f64` cast is lossless, so that stays the fast path;
            // above it the cast rounds and would report `3**34` equal to
            // `(3**34).to_f`, which MRI says is false.
            (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => {
                if x.unsigned_abs() <= (1u64 << 53) {
                    *x as f64 == *y
                } else {
                    Self::exact_float_int(&Value::Float(*y))
                        .is_some_and(|f| f == num_bigint::BigInt::from(*x))
                }
            }
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Undef, Value::Undef) => true,
            // Rational equality (also equal to an integer of the same value —
            // `as_rational` converts an Integer of either width, which is why
            // this arm comes BEFORE the BigInt one: `2**64 == Rational(2**64, 1)`
            // has a BigInt on the left and only the rational path can answer it).
            // Against a Float, MRI's `Rational#==` really does go through
            // `to_f`, so `Rational(1, 3) == 1.0/3` is true.
            _ if matches!(self.obj(a), Some(RObj::Rational(_)))
                || matches!(self.obj(b), Some(RObj::Rational(_))) =>
            {
                match (self.as_rational(a), self.as_rational(b)) {
                    (Some(x), Some(y)) => x == y,
                    _ => match (self.as_f64(a), self.as_f64(b)) {
                        (Some(x), Some(y)) => x == y,
                        _ => false,
                    },
                }
            }
            // Integer equality across the i64/BigInt boundary (a promoted BigInt
            // is never equal to an i64, since it never holds an in-range value,
            // but two BigInts or a BigInt vs Int compare by value). Against a
            // Float, MRI compares EXACTLY rather than converting the Integer —
            // `(10**23) == 1e23` is false where `(10**23).to_f == 1e23` is true
            // — so the Float is the side that has to convert, and it only can
            // when it is finite and has no fractional part.
            _ if matches!(self.obj(a), Some(RObj::BigInt(_)))
                || matches!(self.obj(b), Some(RObj::BigInt(_))) =>
            {
                match (self.as_bigint(a), self.as_bigint(b)) {
                    (Some(x), Some(y)) => x == y,
                    (Some(x), None) => Self::exact_float_int(b).is_some_and(|y| x == y),
                    (None, Some(y)) => Self::exact_float_int(a).is_some_and(|x| x == y),
                    (None, None) => false,
                }
            }
            // A Complex equals a real number when its imaginary part is zero and
            // its real part is equal: `Complex(1, 0) == 1.0` is true.
            _ if matches!(self.obj(a), Some(RObj::Complex { .. }))
                != matches!(self.obj(b), Some(RObj::Complex { .. })) =>
            {
                let (cx, real) = match self.obj(a) {
                    Some(RObj::Complex { .. }) => (a, b),
                    _ => (b, a),
                };
                match self.complex_parts(cx) {
                    Some((re, im)) => {
                        self.eq_values(&im, &Value::Int(0)) && self.eq_values(&re, real)
                    }
                    None => false,
                }
            }
            _ => {
                let (oa, ob) = (self.obj(a), self.obj(b));
                match (oa, ob) {
                    (Some(RObj::Str(x)), Some(RObj::Str(y))) => x == y,
                    (Some(RObj::Symbol(x)), Some(RObj::Symbol(y))) => x == y,
                    (Some(RObj::Array(x)), Some(RObj::Array(y))) => {
                        // Element-wise through `rb_equal`, not `==`: MRI compares
                        // array elements with `rb_equal`, so `[NaN] == [NaN]` is
                        // true even though `NaN == NaN` is false.
                        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.rb_equal(p, q))
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
                    // Two Regexps are equal when their source and their options
                    // agree — `/a/ == /a/` is true even though each literal
                    // allocates its own object. Options compare as the NORMALIZED
                    // bitmask, not as the flag text, so `/a/im == /a/mi`. (MRI
                    // also compares the encoding; there is no per-Regexp encoding
                    // to compare here, so source and options decide it.)
                    (
                        Some(RObj::Regexp {
                            source: xs,
                            flags: xf,
                            ..
                        }),
                        Some(RObj::Regexp {
                            source: ys,
                            flags: yf,
                            ..
                        }),
                    ) => xs == ys && regex_option_bits(xf) == regex_option_bits(yf),
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
/// Whether a Symbol's name can follow a bare `:` in `inspect` output. MRI
/// (`rb_enc_symname_type`, symbol.c) writes every other name quoted —
/// `:"weird sym"`, `:""`, `:"1a"` — so the form round-trips through `eval`.
pub fn plain_symbol_name(s: &str) -> bool {
    // Every operator method name is writable bare.
    const OPS: &[&str] = &[
        "+", "-", "*", "/", "%", "**", "==", "!=", "<", "<=", ">", ">=", "<=>", "===", "=~", "!~",
        "!", "~", "[]", "[]=", "<<", ">>", "&", "|", "^", "+@", "-@", "`",
    ];
    if OPS.contains(&s) {
        return true;
    }
    // `@ivar` / `@@cvar` / `$gvar` keep their sigil; only a plain name may also
    // carry a trailing `?` (predicate), `!` (bang) or `=` (writer).
    let (sigil, body) = match s.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => match s.strip_prefix('@').or_else(|| s.strip_prefix('$')) {
            Some(rest) => (true, rest),
            None => (false, s),
        },
    };
    let core = match body.chars().last() {
        Some('?' | '!' | '=') if !sigil && body.chars().count() > 1 => &body[..body.len() - 1],
        _ => body,
    };
    let mut cs = core.chars();
    match cs.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    cs.all(|c| c == '_' || c.is_alphanumeric())
}

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
            // Anything MRI does not consider printable is escaped as `\uXXXX`.
            // That is the C0 controls and DEL, the C1 controls, and the line and
            // paragraph separators — a raw U+2028 inside an inspect form would
            // otherwise break the line it is printed on.
            c if (c as u32) < 0x20 || matches!(c as u32, 0x7f..=0x9f | 0x2028 | 0x2029) => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The shortest round-tripping decimal digit string for `|f|`, plus the position
/// of the decimal point relative to it — the `digits` / `decpt` pair MRI gets
/// from `ruby_dtoa(value, 0, 0, ...)`.
///
/// Rust's `{:e}` already emits the shortest representation that round-trips, so
/// the digits are read off it and the exponent shifted by one (`{:e}` puts the
/// point after the first digit, `decpt` puts it before them all).
fn dtoa_shortest(f: f64) -> (String, i32) {
    let s = format!("{:e}", f.abs());
    let (mant, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
    let decpt = exp.parse::<i32>().unwrap_or(0) + 1;
    (mant.chars().filter(|c| *c != '.').collect(), decpt)
}

/// `Float#to_s`, ported from MRI `flo_to_s` (numeric.c).
///
/// The choice between fixed and exponential notation is driven by the *shortest
/// representation*, not by magnitude: a value keeps fixed notation whenever the
/// decimal point falls inside the digit string (`decpt < digs`), however large
/// it is. That is why `1e15` prints as `1.0e+15` but `3333333333333333.5`, which
/// is larger, prints in full.
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
    /// `DBL_DIG` — past this many leading places MRI gives up on fixed notation.
    const DBL_DIG: i32 = 15;
    let (digits, decpt) = dtoa_shortest(f);
    let digs = digits.len() as i32;
    let mut out = if f.is_sign_negative() {
        "-".to_string()
    } else {
        String::new()
    };
    if decpt > 0 {
        if decpt < digs {
            // The point lands between digits: split the string there.
            let (int, frac) = digits.split_at(decpt as usize);
            out.push_str(int);
            out.push('.');
            out.push_str(frac);
            return out;
        }
        if decpt <= DBL_DIG {
            // Every digit is integral; pad out to the point and add a bare `.0`.
            out.push_str(&digits);
            out.extend(std::iter::repeat('0').take((decpt - digs) as usize));
            out.push_str(".0");
            return out;
        }
    } else if decpt > -4 {
        // A small magnitude still prints in full: `0.` then the leading zeros.
        out.push_str("0.");
        out.extend(std::iter::repeat('0').take((-decpt) as usize));
        out.push_str(&digits);
        return out;
    }
    // Exponential form: one digit, a point, the rest (or a filler `0`), then a
    // signed exponent padded to at least two digits.
    out.push_str(&digits[..1]);
    out.push('.');
    if digs > 1 {
        out.push_str(&digits[1..]);
    } else {
        out.push('0');
    }
    out.push_str(&format!("e{:+03}", decpt - 1));
    out
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

/// `Method#parameters` for a WRITTEN parameter list. Ruby fixes the order —
/// required positionals, optional positionals, `*rest`, post-splat required
/// positionals — so the `opt` count identifies which of the pre-splat names carry
/// defaults, and everything after the splat is required again. `kwsplat` is the
/// `**rest` name, or `Some("")` when the collector is present but its name was
/// desugared away (a block's, which the parser rewrites into a capture param).
fn written_params(
    params: &[String],
    splat: Option<usize>,
    opt: usize,
    kwnames: &[String],
    kwreq: &[String],
    kwsplat: Option<&str>,
    blockparam: Option<&str>,
) -> Vec<(&'static str, Option<String>)> {
    let mut out: Vec<(&'static str, Option<String>)> = Vec::new();
    let pre = splat.unwrap_or(params.len());
    for (i, p) in params.iter().enumerate() {
        let name = p.trim_start_matches('*').to_string();
        let kind = if Some(i) == splat {
            "rest"
        } else if i < pre.saturating_sub(opt) || i > pre {
            "req"
        } else {
            "opt"
        };
        // A DESTRUCTURING parameter — `->(a, (b, c)) {}` — has no written name.
        // The parser gives it a synthetic one to bind against, and MRI reports
        // such a parameter as a one-element entry:
        //
        // ```console
        // $ /opt/homebrew/opt/ruby/bin/ruby -e 'p ->(a, (b, c)) {}.parameters'
        // [[:req, :a], [:req]]
        // ```
        let written = (!name.starts_with("__destructure_")).then_some(name);
        out.push((kind, written));
    }
    for k in kwnames {
        let kind = if kwreq.contains(k) { "keyreq" } else { "key" };
        out.push((kind, Some(k.clone())));
    }
    if let Some(ks) = kwsplat {
        let name = ks.trim_start_matches('*');
        out.push(("keyrest", (!name.is_empty()).then(|| name.to_string())));
    }
    if let Some(bp) = blockparam {
        out.push(("block", Some(bp.trim_start_matches('&').to_string())));
    }
    out
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
    // Most are `*Error`; the rest are the handful MRI named otherwise. The
    // `Errno::` family is the one namespaced group MRI defines itself.
    name.ends_with("Error")
        || name.starts_with("Errno::")
        || matches!(
            name,
            "Exception" | "StopIteration" | "SystemExit" | "SignalException" | "Interrupt"
        )
}

/// The direct superclass of a builtin exception class, mirroring MRI's tree
/// (`error.c`). `Exception` itself has no exception parent (it derives from
/// `Object`), and any `*Error` not listed sits directly under `StandardError` —
/// the same default MRI uses for library-defined errors, and what makes a bare
/// `rescue` catch them.
fn builtin_exception_parent(name: &str) -> Option<&'static str> {
    Some(match name {
        "Exception" => return None,
        // Direct children of Exception — deliberately NOT rescued by a bare
        // `rescue`, which is the whole point of keeping them off StandardError.
        "NoMemoryError" | "ScriptError" | "SecurityError" | "SignalException" | "StandardError"
        | "SystemExit" | "SystemStackError" => "Exception",
        "LoadError" | "NotImplementedError" | "SyntaxError" => "ScriptError",
        "Interrupt" => "SignalException",
        // Intermediate parents inside StandardError.
        // Every `Errno::*` is a SystemCallError, which is what makes
        // `rescue SystemCallError` catch a missing file the same way MRI does.
        n if n.starts_with("Errno::") => "SystemCallError",
        "UncaughtThrowError" => "ArgumentError",
        "EOFError" => "IOError",
        "KeyError" | "StopIteration" => "IndexError",
        "ClosedQueueError" => "StopIteration",
        "NoMethodError" => "NameError",
        "FloatDomainError" => "RangeError",
        "FrozenError" => "RuntimeError",
        "NoMatchingPatternKeyError" => "NoMatchingPatternError",
        _ => "StandardError",
    })
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
    run_chunk_seeded(chunk, &[])
}

/// Run a method's body chunk, seeding the leading frame slots with the call's
/// positional args when the method binds its params into slots (`slot_params`,
/// Slice 2). Identical to `run_chunk_on(def.chunk.clone())` when `slot_params` is
/// 0 (every param host-bound).
fn run_method_chunk(def: &MethodDef, args: &[Value]) -> Result<Value, String> {
    let n = def.slot_params as usize;
    if n == 0 {
        return run_chunk_on(def.chunk.clone());
    }
    let seed: Vec<Value> = (0..n)
        .map(|i| args.get(i).cloned().unwrap_or(Value::Undef))
        .collect();
    run_chunk_seeded(def.chunk.clone(), &seed)
}

/// Build the VM for `chunk`, seed its leading frame slots with `slot_seed`
/// (`slot_params` binding, empty for the common case), and run it — via the
/// linked AOT native driver when the chunk carries one, else the interpreter.
fn run_chunk_seeded(chunk: Chunk, slot_seed: &[Value]) -> Result<Value, String> {
    // In a `--build --native` binary each method/block chunk carries a non-zero
    // `native_id` and its AOT-lowered native driver is linked in (see aot.rs). Run
    // that machine code directly on the VM instead of interpreting the ops — the
    // driver still needs the builtins + numeric hook installed (its threaded ops
    // call back through them), just not the interpreter loop or the tracing JIT.
    let native = crate::aot::native_entry(chunk.native_id);
    let mut vm = VM::new(chunk);
    crate::builtins::install(&mut vm);
    vm.set_numeric_hook(std::sync::Arc::new(|op, a, b| {
        crate::builtins::numeric_hook(op, a, b)
    }));
    // Seed the leading frame slots with the caller's positional args before the
    // body runs (native driver or interpreter both read them via `GetSlot`).
    for (i, v) in slot_seed.iter().enumerate() {
        vm.set_slot(i as u16, v.clone());
    }
    let outcome = if let Some(entry) = native {
        // AOT native: the linked driver runs the chunk and stores its result.
        let _ = entry(&mut vm as *mut VM);
        vm.take_aot_result()
    } else if DEBUG_MODE.with(|d| d.get()) {
        // The DAP line marker pauses the interpreter; the tracing JIT would
        // compile hot loops and skip the markers, so it stays off in debug mode.
        vm.set_extension_handler(Box::new(|vm, id, _| {
            if id == ext::DBG_LINE {
                crate::dap::on_debug_line(vm);
            }
        }));
        vm.run()
    } else {
        vm.enable_tracing_jit();
        vm.run()
    };
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
                // A required file always runs at the top level with `self` = the
                // `main` object (heap slot 0), regardless of where `require` was
                // called from. Using nil here broke a top-level `require` inside a
                // file that was itself required from a class body (concurrent-ruby
                // → `require` on nil).
                locals: new_env(),
                block: None,
                self_obj: Value::Obj(0),
                method_name: None,
                def_class: None,
                frame_id: next_frame_id(),
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

/// `:a, :b` — the MRI rendering of a keyword-name list in an `ArgumentError`.
fn kw_list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!(":{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `ArgumentError` MRI raises before a method body runs, or `Ok(())`.
///
/// Three checks, in MRI's order: positional count, then unknown keywords, then
/// missing keywords. Messages match `vm_args.c` exactly — `expected N`,
/// `expected N..M` (with optional params), `expected N+` (with a splat), plus the
/// `; required keyword: x` suffix a signature with required keywords carries.
///
/// This is what makes a wrong call a *raise* rather than a silent nil binding —
/// `def f(x, y); end; f(1)` used to run the body with `y` unbound.
fn check_arity(def: &MethodDef, args: &[Value]) -> Result<(), String> {
    check_call_arity(&ArityFacts::of_method(def), args)
}

/// The parameter-count facts a call is checked against. `def`, `->`, `lambda`,
/// `define_method` and `Method#to_proc` all share this one description so they
/// raise the identical `ArgumentError` and report the identical `arity`.
pub struct ArityFacts<'a> {
    /// Positional params with no default, on both sides of a `*rest`.
    req: u16,
    /// Positional params with a default.
    opt: u16,
    has_rest: bool,
    /// Every declared keyword name (required and optional).
    kwnames: &'a [String],
    /// The keyword params with no default.
    kwreq: &'a [String],
    has_kwrest: bool,
}

impl<'a> ArityFacts<'a> {
    pub fn of_method(def: &'a MethodDef) -> Self {
        ArityFacts {
            req: def.req,
            opt: def.opt,
            has_rest: def.splat.is_some(),
            kwnames: &def.kwparams,
            kwreq: &def.kwreq,
            has_kwrest: def.kwsplat.is_some(),
        }
    }
    pub fn of_proc(def: &'a ProcDef) -> Self {
        ArityFacts {
            req: def.arity.req,
            opt: def.arity.opt,
            has_rest: def.splat.is_some(),
            kwnames: &def.arity.kwnames,
            kwreq: &def.arity.kwreq,
            has_kwrest: def.arity.kwsplat.is_some(),
        }
    }
    /// MRI's `rb_iseq_min_max_arity`: the mandatory count, and the maximum
    /// acceptable positional count (`None` = unlimited, i.e. a `*rest`). A
    /// keyword hash counts as one toward the maximum, and required keywords
    /// contribute exactly one to the minimum however many there are.
    fn min_max(&self) -> (i64, Option<i64>) {
        let min = self.req as i64 + !self.kwreq.is_empty() as i64;
        let max = if self.has_rest {
            None
        } else {
            Some(
                self.req as i64
                    + self.opt as i64
                    + (!self.kwnames.is_empty() || self.has_kwrest) as i64,
            )
        };
        (min, max)
    }
    /// `Proc#arity` / `Method#arity`. A lambda (and a method) reports a negative
    /// `-min-1` whenever its call shape is not a single fixed count; a plain
    /// proc goes negative only for a `*rest`, since it binds everything else
    /// leniently (MRI `rb_proc_arity`).
    pub fn arity_value(&self, strict: bool) -> i64 {
        let (min, max) = self.min_max();
        let fixed = if strict {
            max == Some(min)
        } else {
            max.is_some()
        };
        if fixed {
            min
        } else {
            -min - 1
        }
    }
}

/// Three checks, in MRI's order: positional count, then unknown keywords, then
/// missing keywords.
fn check_call_arity(def: &ArityFacts, args: &[Value]) -> Result<(), String> {
    let wants_kw = !def.kwnames.is_empty() || def.has_kwrest;
    // With keyword params, a trailing Hash argument is the keyword hash and does
    // not count as positional (`bind_params` splits it the same way).
    let (positional, kwhash) = if wants_kw {
        match args.last() {
            Some(v) if with_host(|h| h.as_hash(v)).is_some() => {
                (args.len() - 1, with_host(|h| h.as_hash(v)))
            }
            _ => (args.len(), None),
        }
    } else {
        (args.len(), None)
    };

    let (req, opt) = (def.req as usize, def.opt as usize);
    let ok = if def.has_rest {
        positional >= req
    } else {
        positional >= req && positional <= req + opt
    };
    if !ok {
        let expected = if def.has_rest {
            format!("{req}+")
        } else if opt > 0 {
            format!("{req}..{}", req + opt)
        } else {
            req.to_string()
        };
        // MRI appends the required keywords whenever the signature has any, even
        // when the call did supply them.
        let suffix = match def.kwreq.len() {
            0 => String::new(),
            1 => format!("; required keyword: {}", def.kwreq[0]),
            _ => format!("; required keywords: {}", def.kwreq.join(", ")),
        };
        return Err(crate::builtins::raise_exc(
            "ArgumentError",
            &format!("wrong number of arguments (given {positional}, expected {expected}{suffix})"),
        ));
    }

    if !wants_kw {
        return Ok(());
    }
    // A `**opts` collector absorbs every unlisted keyword, so only a signature
    // without one can have an unknown keyword.
    if !def.has_kwrest {
        let unknown: Vec<String> = kwhash
            .iter()
            .flat_map(|m| m.keys())
            .filter_map(|k| match k {
                RKey::Sym(s) if !def.kwnames.iter().any(|p| p == s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        if !unknown.is_empty() {
            let label = if unknown.len() == 1 {
                "unknown keyword"
            } else {
                "unknown keywords"
            };
            return Err(crate::builtins::raise_exc(
                "ArgumentError",
                &format!("{label}: {}", kw_list(&unknown)),
            ));
        }
    }
    let missing: Vec<String> = def
        .kwreq
        .iter()
        .filter(|k| {
            !kwhash
                .as_ref()
                .is_some_and(|m| m.contains_key(&RKey::Sym((*k).clone())))
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        let label = if missing.len() == 1 {
            "missing keyword"
        } else {
            "missing keywords"
        };
        return Err(crate::builtins::raise_exc(
            "ArgumentError",
            &format!("{label}: {}", kw_list(&missing)),
        ));
    }
    Ok(())
}

/// Invoke a top-level `def` (one in the flat method table, not on any class) on
/// `self_obj` — what a bound `Method` for a top-level method calls.
pub fn run_top_method(
    def: &MethodDef,
    self_obj: Value,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    run_method(def, self_obj, args, block, Some(name.to_string()), None)
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
    // Ruby-level recursion guard: cap the frame depth well below the Rust stack
    // limit so runaway Ruby recursion raises a catchable SystemStackError
    // (host.rs maps "stack level too deep" to it) with a Ruby backtrace, instead
    // of overflowing the native stack and aborting the process.
    if with_host(|h| h.frame_depth()) > 2000 {
        return Err(format!(
            "stack level too deep: {}",
            method_name.as_deref().unwrap_or("(unknown)")
        ));
    }
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
    // A synthetic class/module body (`__class_body__N`) runs with `self` = the
    // class. A `def` directly in its body — including one nested in an `if`/`else`
    // the compiler couldn't hoist — must register on that class, so make the class
    // the active `def` target for the body's duration.
    let class_body_target = match (&method_name, &def_class) {
        (Some(n), Some(c)) if n.starts_with("__class_body__") => Some(c.clone()),
        _ => None,
    };
    check_arity(def, args)?;
    let frame_id = next_frame_id();
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
                frame_id,
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
        if let Some(cls) = &class_body_target {
            b.push(DefTarget::Instance(cls.clone()));
            true
        } else if b.is_empty() {
            false
        } else {
            b.push(DefTarget::None);
            true
        }
    });
    let r = run_method_chunk(def, args);
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
        // A local method-body `return` (untagged) ends this method.
        Some(Signal::Return(v, None)) => Ok(v),
        // A block's non-local `return` targeting THIS activation ends it too;
        // one targeting an outer frame keeps unwinding (re-arm and propagate).
        Some(Signal::Return(v, Some(home))) => {
            if home == frame_id {
                Ok(v)
            } else {
                with_host(|h| h.signal = Some(Signal::Return(v, Some(home))));
                r
            }
        }
        // A `throw` must keep unwinding past this method boundary to reach its
        // `catch`; re-arm the signal so the caller's chunk halts too.
        Some(other @ Signal::Throw(..)) => {
            with_host(|h| h.signal = Some(other));
            r
        }
        // A `break` from a block this method `yield`ed ends the method here, but
        // the value belongs to the call site the block literal was written on —
        // re-arm it so that site's `finish_block_call` produces it. Verified
        // against ruby 4.0.6: `def y; yield; p :afty; :fell; end; p(y { break 3 })`
        // prints `3` and never prints `:afty`. Dropping it (the old `_` arm) made
        // the whole call evaluate to nil.
        Some(other @ Signal::Break(_)) => {
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
        // MRI names the receiver even when the call had none written — a
        // top-level call reports `for main`. Omitting the clause produced a
        // sentence in no MRI's vocabulary, and the two callers that recognise
        // this message both accept the longer form.
        return Err(format!(
            "undefined method '{name}' for {}",
            with_host(|h| h.receiver_phrase(&self_obj))
        ));
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
    let (def, owner) = with_host(|h| h.find_method_owner(class, name)).ok_or_else(|| {
        format!(
            "undefined method '{name}' for {}",
            with_host(|h| h.receiver_phrase(&recv))
        )
    })?;
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
    // If the running method is an alias of a user method, `super` resolves as the
    // original name (Ruby aliases preserve the super binding).
    let method = with_host(|h| h.alias_original(&def_class, &method)).unwrap_or(method);
    // `super` from a singleton/class method (`def self.m`): the receiver is a
    // class ref with no object class, so resolve through the singleton-class
    // ancestry (class methods above `def_class`) rather than the instance chain.
    if with_host(|h| h.object_class(&self_obj)).is_none() {
        if let Some(cls) = with_host(|h| h.classref_name(&self_obj)) {
            if let Some((def, owner)) =
                with_host(|h| h.find_super_class_method(&cls, &def_class, &method))
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
        // `super` from a user override of `Class#subclasses`/`descendants` reaches
        // the native hierarchy walk. Rails stacks overrides (Railtie#subclasses ->
        // DescendantsTracker::ReloadedClassesFiltering#subclasses -> super) that all
        // bottom out here.
        if matches!(method.as_str(), "subclasses" | "descendants") {
            if let Some(cls) = with_host(|h| h.classref_name(&self_obj)) {
                let names = with_host(|h| {
                    if method == "subclasses" {
                        h.direct_subclasses(&cls)
                    } else {
                        h.all_descendants(&cls)
                    }
                });
                let refs: Vec<Value> = names
                    .iter()
                    .map(|n| with_host(|h| h.class_ref(n)))
                    .collect();
                return Ok(with_host(|h| h.new_array(refs)));
            }
            return Ok(with_host(|h| h.new_array(vec![])));
        }
        // `super` from an override of an Exception introspection method (Rails
        // wraps exceptions and overrides these, chaining up via `super`). The
        // native default is the stored value, or empty/nil.
        if matches!(method.as_str(), "backtrace" | "backtrace_locations") {
            let stored = with_host(|h| h.ivar_of(&self_obj, "backtrace"));
            return Ok(if matches!(stored, Value::Undef) {
                with_host(|h| h.new_array(vec![]))
            } else {
                stored
            });
        }
        if method == "cause" {
            return Ok(with_host(|h| h.ivar_of(&self_obj, "cause")));
        }
        // `super` from a user override of `Module#include`/`prepend`/`extend`
        // (self is the class, args are the modules) performs the real mixin. e.g.
        // concurrent-ruby's ReInclude#include calls `super(*modules)` then replays
        // the include into dependents.
        if matches!(method.as_str(), "include" | "prepend" | "extend") {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            if let Some(cls) = with_host(|h| h.classref_name(&self_obj)) {
                for a in &args {
                    if let Some(m) = with_host(|h| h.classref_name(a)) {
                        with_host(|h| h.class_mixin(&cls, &m, &method));
                    }
                }
            }
            return Ok(self_obj.clone());
        }
        // `super` from an override of `Module#append_features`/`prepend_features`
        // (self is the module, arg is the base) performs the real mixin: the
        // default native behavior adds the module's instance methods to the base.
        // ActiveSupport::Concern#append_features calls `super` to do exactly this.
        if matches!(method.as_str(), "append_features" | "prepend_features") {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            if let (Some(module), Some(base)) = (
                with_host(|h| h.classref_name(&self_obj)),
                args.first().and_then(|a| with_host(|h| h.classref_name(a))),
            ) {
                let kind = if method == "append_features" {
                    "include"
                } else {
                    "prepend"
                };
                with_host(|h| h.class_mixin(&base, &module, kind));
            }
            return Ok(Value::Undef);
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
        // A user override of a universal type query that calls `super` gets the
        // native behavior. Re-dispatching would re-enter the override (infinite
        // recursion), so compute it directly here (mustermann's `Node#is_a?`
        // normalizes a Symbol type then `super(type)`).
        if matches!(method.as_str(), "is_a?" | "kind_of?" | "instance_of?") {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            let ans = args
                .first()
                .and_then(|arg| {
                    with_host(|h| h.classref_name(arg)).map(|cname| {
                        if method == "instance_of?" {
                            with_host(|h| h.class_of(&self_obj)) == cname
                        } else {
                            with_host(|h| h.is_a(&self_obj, &cname))
                        }
                    })
                })
                .unwrap_or(false);
            return Ok(Value::Bool(ans));
        }
        // `super` from a user override of `respond_to?` (`name == :x || super`):
        // the default `Object#respond_to?` reports whether the object actually
        // has the named method. Compute directly to avoid re-entering the override.
        if method == "respond_to?" {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            let mname = args
                .first()
                .map(|a| with_host(|h| h.to_s(a)))
                .unwrap_or_default();
            let cls = with_host(|h| h.class_of(&self_obj));
            let ans = with_host(|h| {
                h.is_method_defined(&cls, &mname) || h.attr_access(&cls, &mname).is_some()
            });
            return Ok(Value::Bool(ans));
        }
        // `super` from a `method_missing` override: the default `BasicObject#
        // method_missing` raises NoMethodError for the unhandled name. A
        // `Delegator#method_missing` calls `super` when the wrapped object does
        // not respond to the forwarded method.
        if method == "method_missing" {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            let mname = args
                .first()
                .map(|a| with_host(|h| h.to_s(a)))
                .unwrap_or_default();
            let target = with_host(|h| match h.classref_name(&self_obj) {
                Some(c) => format!("class {c}"),
                None => format!("an instance of {}", h.class_of(&self_obj)),
            });
            return Err(crate::builtins::raise_exc(
                "NoMethodError",
                &format!("undefined method '{mname}' for {target}"),
            ));
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
        // `super` reaching a *native* builtin method: a user override on a
        // subclass of (or reopening of) Hash/Array/String/etc. calling `super`
        // (`class Params < Hash; def delete(k); …; super; end`). Dispatch straight
        // to the builtin table by the receiver's raw type, bypassing user methods
        // so we don't re-enter the override. Only attempt when the raw type
        // differs from the linearization root (i.e. there is a builtin base).
        let base = with_host(|h| h.dispatch_class(&self_obj));
        if base != recv_class {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            let block = match block_override.clone() {
                Some(b) => Some(b),
                None => with_host(|h| h.cur_scope().block.clone()),
            };
            let r = crate::builtins::dispatch_by_type(&base, &self_obj, &method, &args, block);
            match &r {
                Ok(_) => return r,
                Err(e) if !e.starts_with("undefined method") => return r,
                _ => {}
            }
        }
        // `super` reaching an alias whose target is a native method: `alias
        // send_action send` in AbstractController::Base, which Rails'
        // BasicImplicitRender#send_action overrides with a `super` call. The alias
        // is stored as a native marker (not a user MethodDef), so `find_super`
        // above misses it — resolve the native target and dispatch it. (Unlike
        // `native_kernel_alias`, this accepts any native target, e.g. `send`.)
        if let Some(native) = with_host(|h| {
            h.find_alias(&recv_class, &method)
                .and_then(|t| RubyHost::native_alias_target(&t).map(str::to_string))
        }) {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            let block = match block_override.clone() {
                Some(b) => Some(b),
                None => with_host(|h| h.cur_scope().block.clone()),
            };
            return crate::builtins::dispatch(&self_obj, &native, &args, block);
        }
        // `super` reaching a native attr accessor above the override: Rails'
        // AbstractController::Base does `attr_internal :response_body`, and
        // ActionController::Metal#response_body= (a real method override) calls
        // `super` to reach that base writer. `find_super` only walks real methods,
        // so resolve the attr accessor and read/write its field.
        if let Some((field, is_writer)) = with_host(|h| h.attr_access(&recv_class, &method)) {
            let args = explicit_args.clone().unwrap_or_else(|| cur_args.clone());
            return Ok(if is_writer {
                let v = args.first().cloned().unwrap_or(Value::Undef);
                with_host(|h| h.set_ivar_of(&self_obj, &field, v.clone()));
                v
            } else {
                with_host(|h| h.ivar_of(&self_obj, &field))
            });
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

/// Infer a Ruby exception class from a bare error message produced by a builtin
/// that returned `Err(msg)` without an explicit `raise_exc`. Mirrors the class
/// MRI would raise for the same condition; defaults to `RuntimeError` (Ruby's
/// default for a bare `raise "msg"`).
fn infer_exc_class(msg: &str) -> String {
    let m = msg;
    if m.starts_with("undefined method") || m.starts_with("undefined singleton method") {
        "NoMethodError"
    } else if m.starts_with("undefined local variable")
        || m.starts_with("uninitialized constant")
        || m.starts_with("undefined method 'const")
    {
        "NameError"
    } else if m.starts_with("wrong number of arguments") || m.contains("wrong number of arguments")
    {
        "ArgumentError"
    } else if m.starts_with("can't modify frozen") {
        "FrozenError"
    } else if m.starts_with("no implicit conversion")
        || m.contains("can't be coerced")
        || m.contains("is not a class")
        || m.contains("is not a module")
        || m.contains("is not a symbol")
    {
        "TypeError"
    } else if m.contains("divided by 0") {
        "ZeroDivisionError"
    } else if m.starts_with("key not found") || m.starts_with("index") && m.contains("out of") {
        "KeyError"
    } else if m.starts_with("No such file") || m.contains("Errno::") {
        "IOError"
    } else if m.starts_with("stack level too deep") {
        "SystemStackError"
    } else {
        "RuntimeError"
    }
    .to_string()
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
        let mut exc = with_host(|h| h.take_pending_exc());
        // A Rust-level `Err(msg)` with no pending-exception object still marks a
        // raised Ruby exception — many builtins `return Err(msg)` directly rather
        // than routing through `raise_exc`. Without an object, `rescue => e` would
        // bind `e` to nil (sinatra's `handle_exception!` then crashes on
        // `boom.message`). Synthesize one, inferring the class from the message so
        // `rescue NoMethodError`/etc. still matches.
        if exc.is_none() {
            let class = infer_exc_class(&e);
            let v = with_host(|h| h.new_exception(&class, &e));
            exc = Some(v);
        }
        let exc_class = exc
            .as_ref()
            .and_then(|v| with_host(|h| h.object_class(v)))
            .unwrap_or_else(|| "StandardError".to_string());
        let excv = exc.clone().unwrap_or(Value::Undef);
        let mut handled = false;
        let mut retrying = false;
        for rd in &bd.rescues {
            // A bare `rescue` (no classes, no splat) catches StandardError and
            // nothing above it — `SystemExit`, `LoadError`, `NotImplementedError`
            // and friends deliberately fall through it, as in MRI.
            let is_bare = rd.classes.is_empty()
                && rd.splat.is_none()
                && with_host(|h| h.exc_matches(&exc_class, "StandardError"));
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
            frame_id: next_frame_id(),
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
                "attempt to yield on a not resumed fiber",
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
            "attempt to resume a terminated fiber",
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
    let mut fiber_ctx = with_host(|h| h.install_fiber_ctx(caller_ctx));

    let out = match out {
        corosensei::CoroutineResult::Yield(y) => Ok(y),
        corosensei::CoroutineResult::Return(r) => {
            with_fibers(|fibers| fibers[id as usize].done = true);
            if r.is_err() {
                // The raise happened on the fiber's side of the swap, so the
                // exception OBJECT sits in the fiber's context while only the
                // message string rides out in the `Err`. Hand the object back to
                // the caller, or `rescue` would rebuild it from that string and
                // every `raise TypeError` would arrive as a bare RuntimeError.
                if let Some(exc) = fiber_ctx.pending_exc.take() {
                    with_host(|h| h.set_pending_exc(exc));
                }
            }
            r // block's value, or a propagated raise
        }
    };
    with_fibers(|fibers| {
        fibers[id as usize].ctx = fiber_ctx;
        fibers[id as usize].coro = Some(coro);
    });
    out
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

/// Write `s` as program output on stdout — the funnel `puts`/`print`/`p` and
/// the other Kernel writers use, so an embedder's capture catches them all.
pub fn write_stdout(s: &str) {
    with_host(|h| h.write_out(s, false));
}

/// Write `s` as program output on stderr (`warn`, `$stderr.write`).
pub fn write_stderr(s: &str) {
    with_host(|h| h.write_out(s, true));
}

/// `IO#write` for one already-stringified chunk; returns the byte count written.
pub fn io_write_str(v: &Value, s: &str) -> Result<usize, String> {
    use std::io::Write;
    let id = io_id(v).ok_or("not an IO")?;
    // The standard streams route through the host's output funnel rather than
    // the process fds, so a capturing embedder sees them.
    if with_host(|h| matches!(h.io_handles.get(id as usize), Some(IoCell::Stdout))) {
        write_stdout(s);
        return Ok(s.len());
    }
    if with_host(|h| matches!(h.io_handles.get(id as usize), Some(IoCell::Stderr))) {
        write_stderr(s);
        return Ok(s.len());
    }
    with_host(|h| match h.io_handles.get_mut(id as usize) {
        Some(IoCell::Stdout) | Some(IoCell::Stderr) => unreachable!("handled above"),
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

thread_local! {
    /// Nested Ruby-level `Proc`/block bodies currently running on this thread.
    /// See the guard in [`call_proc_self_ctx`].
    static PROC_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The same activation ceiling `run_method` applies to `def` bodies, so block
/// recursion and method recursion fail at the same depth.
const PROC_DEPTH_LIMIT: usize = 2000;

/// Counts one nested block activation for as long as it is alive. A `Drop`
/// guard rather than a manual decrement because the body has many early exits
/// (`break`/`return` signals, raised exceptions, the delegating `ProcKind`s),
/// and a leaked increment would make an unrelated later call raise.
struct ProcDepth;

impl ProcDepth {
    fn enter() -> Result<Self, String> {
        let depth = PROC_DEPTH.with(|c| {
            let next = c.get() + 1;
            c.set(next);
            next
        });
        if depth > PROC_DEPTH_LIMIT {
            PROC_DEPTH.with(|c| c.set(c.get() - 1));
            return Err("stack level too deep: block".to_string());
        }
        Ok(ProcDepth)
    }
}

impl Drop for ProcDepth {
    fn drop(&mut self) {
        PROC_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
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
    call_proc_self_ctx(proc_val, args, self_override, None, None)
}

/// Call a proc with a block of its own, bound to its `&blk` parameter — what
/// `Proc#call`/`#()`/`#[]`/`#yield` do when handed a block (`->(&b){ b.call }`).
pub fn call_proc_block(
    proc_val: &Value,
    args: &[Value],
    block: Option<Value>,
) -> Result<Value, String> {
    call_proc_self_ctx(proc_val, args, None, None, block)
}

/// Like [`call_proc_self`], but `method_ctx = Some((name, owner))` marks the proc
/// as running as a `define_method`-created instance method: `super` inside the
/// body then resolves against `name` in `owner`'s ancestry (not the proc's
/// defining scope). Without this, `define_method(:m) { super() }` would `super`
/// on the enclosing `__class_body__`.
pub fn call_proc_self_ctx(
    proc_val: &Value,
    args: &[Value],
    self_override: Option<&Value>,
    method_ctx: Option<(String, String)>,
    passed_block: Option<Value>,
) -> Result<Value, String> {
    // A block body does NOT push a `Frame` — it swaps `active_scope` — so
    // `run_method`'s frame-depth guard cannot see recursion that goes through
    // `Proc#call`, a `define_method` body, or a Hash default proc. Those paths
    // ran until the native stack overflowed and the process aborted:
    //
    //   $ ruby -e 'h = Hash.new { |hh, k| hh[k] }; h[:a]'
    //   fatal runtime error: stack overflow, aborting
    //
    // where MRI raises a rescuable SystemStackError. This counter closes that
    // hole at the same 2000 activations the method guard uses, an order of
    // magnitude below where the native stack actually gives out.
    let _depth = ProcDepth::enter()?;
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
        Some(RObj::Method { recv, name, .. }) => {
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
        // The endless-Range generator body: count up forever until the yielder's
        // limit breaks out.
        Some(RObj::SeqProc(lo)) => {
            let Some(yielder) = args.first().cloned() else {
                return Err("no yielder is available".to_string());
            };
            let mut i = lo;
            loop {
                crate::builtins::dispatch(&yielder, "<<", &[Value::Int(i)], None)?;
                if has_pending_signal() {
                    return Ok(Value::Undef);
                }
                i += 1;
            }
        }
        // The limitless-`step` generator body: add `by` forever until the
        // yielder's limit breaks out. `+` keeps the sequence in the receiver's
        // numeric class and promotes on overflow, as MRI's does.
        Some(RObj::StepProc { from, by, float }) => {
            let Some(yielder) = args.first().cloned() else {
                return Err("no yielder is available".to_string());
            };
            if float {
                let to_f = |v: &Value| -> Result<f64, String> {
                    match crate::builtins::dispatch(v, "to_f", &[], None)? {
                        Value::Float(f) => Ok(f),
                        other => Ok(match other {
                            Value::Int(n) => n as f64,
                            _ => 0.0,
                        }),
                    }
                };
                let (base, unit) = (to_f(&from)?, to_f(&by)?);
                let mut i = 0.0f64;
                loop {
                    let v = Value::Float(i * unit + base);
                    crate::builtins::dispatch(&yielder, "<<", &[v], None)?;
                    if has_pending_signal() {
                        return Ok(Value::Undef);
                    }
                    i += 1.0;
                }
            }
            let mut cur = from;
            loop {
                crate::builtins::dispatch(&yielder, "<<", std::slice::from_ref(&cur), None)?;
                if has_pending_signal() {
                    return Ok(Value::Undef);
                }
                cur = crate::builtins::dispatch(&cur, "+", std::slice::from_ref(&by), None)?;
            }
        }
        // A derived generator body: pull the source in batches and forward the
        // reshaped values, so the transform never materializes an infinite source.
        Some(RObj::DeriveProc { src, kind }) => {
            let Some(yielder) = args.first().cloned() else {
                return Err("no yielder is available".to_string());
            };
            return crate::builtins::drive_derived(&src, &kind, &yielder);
        }
        // A `Symbol#to_proc` proc used as a block value: send the symbol's method
        // to the first argument.
        Some(RObj::SymProc(s)) => {
            return match args.split_first() {
                Some((recv, rest)) => crate::builtins::dispatch(recv, &s, rest, None),
                None => Err("no receiver given".to_string()),
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
                // The base keeps the original's lambda-ness, so currying a lambda
                // and then over-applying it (`f.curry.call(1, 2, 3)`) still raises.
                let base = with_host(|h| {
                    h.alloc(RObj::Proc {
                        template,
                        scope: scope.clone(),
                        is_lambda,
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
        ProcKind::MethodCurried {
            target,
            arity,
            collected,
        } => {
            let mut all = collected.clone();
            all.extend_from_slice(args);
            if all.len() >= arity {
                return call_proc(&target, &all);
            }
            return Ok(with_host(|h| {
                h.alloc(RObj::Proc {
                    template,
                    scope: scope.clone(),
                    is_lambda: true,
                    kind: ProcKind::MethodCurried {
                        target: target.clone(),
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

    // A lambda — and a `define_method` body, which MRI also gives method
    // semantics — is arity-checked before its body runs, exactly like a `def`.
    // A plain block is lenient: it binds missing params to nil and drops extras.
    let strict = is_lambda || method_ctx.is_some();
    if strict {
        check_call_arity(&ArityFacts::of_proc(&def), args)?;
    }

    // Auto-splat: a block with more than one parameter slot destructures a single
    // array argument — `pairs.each { |k, v| … }`, and also `{ |first, *rest| … }`.
    // A lone `*rest` (one slot) does not auto-splat. A lambda never auto-splats:
    // `[[1,2]].map(&->(x, y){ … })` raises in MRI rather than unpacking the pair.
    let bound: Vec<Value> = if !strict && def.params.len() > 1 && args.len() == 1 {
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
    // `&blk` captures the block this proc was called with (nil if none). It is
    // deliberately not a positional param — see the parser's `block_param`.
    if let Some(bp) = &def.arity.blockparam {
        let v = passed_block.clone().unwrap_or(Value::Undef);
        child.lock().unwrap().vars.insert(bp.clone(), v);
    }
    // The block's "home": the method activation it was defined in. A non-local
    // `return` from this block unwinds to that frame.
    let home_frame = scope.frame_id;
    let mut block_scope = Scope {
        locals: child,
        self_obj: self_override
            .cloned()
            .unwrap_or_else(|| scope.self_obj.clone()),
        ..scope
    };
    // Running as a `define_method` method: rebind the super context so `super`
    // resolves against this method name in the owner's ancestry.
    if let Some((name, owner)) = method_ctx {
        block_scope.method_name = Some(name);
        block_scope.def_class = Some(owner);
    }
    let prev_active = with_host(|h| h.active_scope.replace(block_scope));
    // `redo` re-runs the body from the top with the SAME scope — the params keep
    // their bound values and any local the body already assigned survives (MRI
    // does not re-bind either), so the child env is deliberately not rebuilt.
    let r = loop {
        let r = run_chunk_on(def.chunk.clone());
        if r.is_err() || !take_redo_signal() {
            break r;
        }
    };
    with_host(|h| {
        h.active_scope = prev_active;
    });
    // A `next` inside the block becomes the block's value; break/return propagate.
    let sig = with_host(|h| h.signal.take());
    match sig {
        Some(Signal::Next(v)) => Ok(v),
        // In a lambda, `return` and `break` are local — they end the lambda and
        // become its value (MRI lambda semantics).
        Some(Signal::Return(v, _)) | Some(Signal::Break(v)) if is_lambda => Ok(v),
        // A plain block's `return` is non-local: tag the still-untagged signal
        // with this block's home frame so it unwinds to its defining method,
        // passing through any intermediate yielder frames. An already-tagged
        // return (from a nested block) keeps its original target.
        Some(Signal::Return(v, None)) => {
            with_host(|h| h.signal = Some(Signal::Return(v, Some(home_frame))));
            r
        }
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
    with_host(|h| h.signal = Some(Signal::Return(v, None)));
}
pub fn raise_signal_retry() {
    with_host(|h| h.signal = Some(Signal::Retry));
}
pub fn raise_signal_redo() {
    with_host(|h| h.signal = Some(Signal::Redo));
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
/// Consume a pending `redo` signal, returning whether one was set. Used both by
/// `call_proc` (to re-run a block body) and by the native-loop handoff op.
pub fn take_redo_signal() -> bool {
    with_host(|h| {
        if matches!(h.signal, Some(Signal::Redo)) {
            h.signal = None;
            true
        } else {
            false
        }
    })
}
thread_local! {
    /// Set by `MKPROC`, cleared by the very next block-carrying call op: "the
    /// block operand about to be consumed is a LITERAL written at that call
    /// site", which is exactly the frame MRI targets a `break` at.
    ///
    /// The compiler emits `MKPROC` as the last operand before its call op (both
    /// in `compile_call` and for `super { ... }`), and only for a block literal
    /// — a forwarded `&expr` compiles the expression instead — so this flag is
    /// always read by the owning call and never by a forwarding one.
    static BLOCK_LITERAL_CALL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `MKPROC` ran: the next block-carrying call op owns any `break` its block
/// raises.
pub fn mark_block_literal() {
    BLOCK_LITERAL_CALL.with(|c| c.set(true));
}

/// Read and clear the block-literal marker. Called at the *entry* of a
/// block-carrying call handler, before dispatch, since running the block can
/// set the flag again for calls nested inside it.
pub fn take_block_literal() -> bool {
    BLOCK_LITERAL_CALL.with(|c| c.replace(false))
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
/// Consume a pending `next` signal, returning its value. Any other signal is
/// left in place so it keeps unwinding.
pub fn take_next() -> Option<Value> {
    with_host(|h| match &h.signal {
        Some(Signal::Next(_)) => match h.signal.take() {
            Some(Signal::Next(v)) => Some(v),
            _ => None,
        },
        _ => None,
    })
}
/// Whether the pending signal is a `break`, `next` or `redo` — the three a
/// native loop owns and will re-dispatch itself, rather than letting it unwind
/// the chunk.
pub fn pending_signal_is_loop_flow() -> bool {
    with_host(|h| {
        matches!(
            h.signal,
            Some(Signal::Break(_)) | Some(Signal::Next(_)) | Some(Signal::Redo)
        )
    })
}
/// Whether a `break` signal is pending, without consuming it.
pub fn peek_break() -> Value {
    with_host(|h| Value::Bool(matches!(h.signal, Some(Signal::Break(_)))))
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
