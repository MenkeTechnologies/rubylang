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
    Match,  // =~
    NMatch, // !~
    And, // &&
    Or,  // ||
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
    Global,   // $foo
    Const,    // Foo
}

/// A block literal attached to a method call (`do |x| … end` or `{ |x| … }`).
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub params: Vec<String>,
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
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
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
    /// `yield args` — invoke the block passed to the enclosing method.
    Yield(Vec<Expr>),
    /// `super` (args `None` = forward the current method's args) / `super(args)`.
    Super(Option<Vec<Expr>>),
    /// A splat argument/element: `*expr` in a call or array literal.
    Splat(Box<Expr>),
    /// A lambda literal `->(params) { body }` (a Proc value).
    Lambda(Block),
    /// A regex literal `/pattern/flags`.
    Regex(String, String),
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
