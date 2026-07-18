//! Ruby abstract syntax tree.
//!
//! Faithful to Ruby's surface grammar as far as rubylang lowers it today: every
//! node here has a direct lowering in `compiler.rs`. The tree is deliberately
//! expression-oriented — in Ruby nearly everything (`if`, `while`, `begin`,
//! assignment) yields a value — so `Stmt` is a thin wrapper and most of the
//! grammar lives in `Expr`.

/// A binary operator with a native fusevm lowering (arithmetic/comparison/logic
/// run in the VM, so JIT applies; only method-dispatch operators go to the host).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Cmp,    // <=>
    CaseEq, // ===
    Match,  // =~
    NMatch, // !~
    And,    // &&
    Or,     // ||
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

/// The kind of a name reference / assignment target, from its sigil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Local,    // foo
    Instance, // @foo
    Class,    // @@foo
    Global,   // $foo
    Const,    // Foo
}

/// A block literal attached to a method call (`do |x| … end` or `{ |x| … }`).
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub params: Vec<String>,
    /// Index into `params` of a `*rest` splat parameter, if any (collects the
    /// surplus positional args into an array).
    pub splat: Option<usize>,
    pub body: Vec<Expr>,
}

/// A Ruby expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Nil,
    True,
    False,
    Int(i64),
    Float(f64),
    /// A string with interpolation: alternating literal/embedded segments.
    Str(Vec<StrPart>),
    Symbol(String),
    Array(Vec<Expr>),
    /// key/value pairs; `k => v` and `k: v` both land here.
    Hash(Vec<(Expr, Expr)>),
    /// `lo..hi` (exclusive=false) / `lo...hi` (exclusive=true).
    /// `lo..hi`. Either bound may be absent for a beginless (`..hi`) or endless
    /// (`lo..`) range.
    Range {
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
        exclusive: bool,
    },

    /// A name read (`foo`, `@foo`, `$foo`, `Foo`).
    Var(VarKind, String),
    /// `target op= value` is desugared in the parser to `Assign(target, Bin(...))`.
    Assign(Box<Expr>, Box<Expr>),
    /// `a, b, c = 1, 2, 3` (or `a, b = arr`) — parallel assignment.
    MultiAssign {
        targets: Vec<Expr>,
        values: Vec<Expr>,
    },

    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),

    /// `cond ? then : else`, and `if`/`unless`/modifier forms.
    If {
        cond: Box<Expr>,
        then: Vec<Expr>,
        elifs: Vec<(Expr, Vec<Expr>)>,
        els: Option<Vec<Expr>>,
    },
    /// `while`/`until` (until is parsed as `while !cond`).
    While {
        cond: Box<Expr>,
        body: Vec<Expr>,
    },
    /// `begin … end while cond` / `begin … end until cond` — a post-test loop:
    /// the body runs at least once, then the condition is checked (until is
    /// parsed as `while !cond`).
    DoWhile {
        cond: Box<Expr>,
        body: Vec<Expr>,
    },
    /// `for v in iter … end`.
    For {
        var: String,
        iter: Box<Expr>,
        body: Vec<Expr>,
    },
    /// `case subj; when …; else …; end`.
    Case {
        subject: Box<Expr>,
        whens: Vec<(Vec<Expr>, Vec<Expr>)>,
        els: Option<Vec<Expr>>,
    },
    /// `case subj; in pattern [if guard]; body; … end` — structural pattern
    /// matching (Ruby 3). An absent `else` raises `NoMatchingPatternError`.
    CaseIn {
        subject: Box<Expr>,
        clauses: Vec<InClause>,
        els: Option<Vec<Expr>>,
    },

    /// A call. `recv` is `None` for a bare/self call (`puts x`, `foo`).
    Call {
        recv: Option<Box<Expr>>,
        name: String,
        args: Vec<Expr>,
        block: Option<Block>,
    },
    /// `recv[idx]` — sugar for `recv.[](idx)` but lowered directly.
    Index(Box<Expr>, Vec<Expr>),

    /// `def name(params) … end`. `singleton` is true for `def self.name` (a
    /// class method).
    Def {
        name: String,
        params: Vec<Param>,
        body: Vec<Expr>,
        singleton: bool,
    },
    /// `class Name [< Super] … end`.
    Class {
        name: String,
        superclass: Option<String>,
        body: Vec<Expr>,
    },
    /// `module Name … end` (treated as a namespace of methods for now).
    Module {
        name: String,
        body: Vec<Expr>,
    },
    /// `self`.
    SelfExpr,

    /// `begin … rescue [Class] [=> e] … ensure … end`.
    Begin {
        body: Vec<Expr>,
        rescues: Vec<Rescue>,
        ensure: Option<Vec<Expr>>,
    },

    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Next(Option<Box<Expr>>),
    /// `retry` — restart the enclosing `begin` body from a `rescue` clause.
    Retry,
    /// `yield args` — invoke the block passed to the enclosing method.
    Yield(Vec<Expr>),
    /// `defined?(expr)` — a description string of the operand (or nil), without
    /// evaluating it.
    Defined(Box<Expr>),
    /// `super` (args `None` = forward the current method's args) / `super(args)`.
    Super(Option<Vec<Expr>>),
    /// A splat argument/element: `*expr` in a call or array literal.
    Splat(Box<Expr>),
    /// A lambda literal `->(params) { body }` (a Proc value).
    Lambda(Block),
    /// A regex literal `/pattern/flags`.
    Regex(String, String),
    /// `class << self … end` — a singleton-class body. Its `def`s become class
    /// (singleton) methods of the enclosing class, equivalent to `def self.x`.
    SingletonClass(Vec<Expr>),
}

/// One `in pattern [if/unless guard]` clause of a `case/in`.
#[derive(Debug, Clone, PartialEq)]
pub struct InClause {
    pub pattern: Pattern,
    /// A trailing `if cond` (or `unless cond`, stored pre-negated) guard.
    pub guard: Option<Expr>,
    pub body: Vec<Expr>,
}

/// A structural pattern for `case/in`.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// A literal / range / anything matched with `===` (`in 5`, `in 1..10`).
    Value(Expr),
    /// `^expr` — match the subject against a pinned value with `==`.
    Pin(Expr),
    /// A local-variable binding (`in x`); `_` is a wildcard that binds nothing.
    Bind(String),
    /// `*rest` inside an array pattern (name `None` for a bare `*`).
    Splat(Option<String>),
    /// `[p0, p1, *rest, pk]`.
    Array(Vec<Pattern>),
    /// `{key: subpattern, key2:, **rest}` — a `None` subpattern is the `key:`
    /// shorthand that binds `key`. The [`HashRest`] records the trailing
    /// `**rest` / `**` / `**nil` (or its absence).
    Hash(Vec<(String, Option<Pattern>)>, HashRest),
    /// A class match (`in Integer`); the optional inner pattern is `Const[...]` /
    /// `Const(...)` deconstruction.
    Const(String, Option<Box<Pattern>>),
    /// `pattern => name` — match, then bind the whole subject to `name`.
    As(Box<Pattern>, String),
    /// `p1 | p2 | …` — alternative patterns.
    Or(Vec<Pattern>),
}

/// The trailing double-splat of a hash pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum HashRest {
    /// No `**` at all. Closed (no other keys) only when the pattern is empty
    /// (`{}`); otherwise extra keys are allowed.
    None,
    /// `**` or `**name` — other keys are allowed (and would collect into `name`).
    Splat(Option<String>),
    /// `**nil` — no keys other than those listed may be present.
    Nil,
}

/// One segment of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    Interp(Box<Expr>),
}

/// A method parameter: name, an optional default expression, whether it is a
/// splat (`*rest`) collecting the remaining positional args, and whether it is a
/// keyword parameter (`name:` / `name: default`).
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub default: Option<Expr>,
    pub splat: bool,
    pub keyword: bool,
    /// `**opts` — collects unmatched keyword arguments into a hash.
    pub kwsplat: bool,
    /// `&blk` — captures the passed block as a Proc.
    pub block: bool,
}

/// One `rescue` clause of a `begin`/`rescue` block.
#[derive(Debug, Clone, PartialEq)]
pub struct Rescue {
    /// Exception class names to match (empty = catch StandardError/any).
    pub classes: Vec<String>,
    /// Optional `=> name` binding for the caught exception.
    pub binding: Option<String>,
    pub body: Vec<Expr>,
}

/// A top-level statement. Ruby is expression-oriented; this is a thin wrapper so
/// the parser can produce a `Vec<Stmt>` program with per-statement lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub expr: Expr,
    pub line: u32,
}
