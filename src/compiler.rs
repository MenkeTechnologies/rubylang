//! Lower the Ruby AST to `fusevm::Chunk`.
//!
//! Arithmetic, comparison and bit operators lower to native fusevm ops so the
//! JIT can trace them; the strict numeric hook (host) supplies Ruby semantics
//! for non-numeric operands. Everything Ruby-specific — variable access, method
//! dispatch, object construction, `yield` — lowers to a `CallBuiltin` that lands
//! in `builtins.rs`.
//!
//! Conditions are normalized through the `TRUTHY` builtin before a native
//! `JumpIfFalse`, because Ruby's truthiness (only `nil`/`false` are falsy — `0`
//! and `""` are true) differs from fusevm's default numeric truthiness.

use crate::ast::*;
use crate::host::{ops, BeginDef, ClassDef, MethodDef, ProcDef, RescueDef};
use fusevm::{Chunk, ChunkBuilder, Op, Value};
use std::sync::Arc;

thread_local! {
    /// Set by `crate::compile` from the source's `# frozen_string_literal:` magic
    /// comment before each file compiles; read when lowering string literals so a
    /// non-interpolated literal freezes. Per-file: `crate::compile` sets it every
    /// call, and every `require` funnels through there, so each file gets its own.
    static FROZEN_STR_LITERALS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the file currently being compiled froze its string literals.
pub fn frozen_string_literals() -> bool {
    FROZEN_STR_LITERALS.with(|c| c.get())
}

/// Set the frozen-string-literal flag for the next compilation (one file).
pub fn set_frozen_string_literals(on: bool) {
    FROZEN_STR_LITERALS.with(|c| c.set(on));
}

/// The full output of compiling a program.
pub struct Program {
    pub main: Chunk,
    pub methods: Vec<(String, MethodDef)>,
    pub classes: Vec<(String, ClassDef)>,
    pub begins: Vec<BeginDef>,
    pub procs: Vec<ProcDef>,
}

/// Break/next/redo jump fixups for a native `while` loop.
struct LoopCtx {
    start: usize,
    /// First op of the loop *body* — where `redo` jumps back to. For `while`/
    /// `until` this is past the condition test (a `redo` must not re-test it);
    /// for the post-test `begin … end while` form it is the same as `start`.
    /// Filled in once the condition has been emitted.
    body_start: usize,
    breaks: Vec<usize>,
    nexts: Vec<usize>,
    redos: Vec<usize>,
}

#[derive(Default)]
pub struct Compiler {
    methods: Vec<(String, MethodDef)>,
    classes: Vec<(String, ClassDef)>,
    begins: Vec<BeginDef>,
    procs: Vec<ProcDef>,
    loops: Vec<LoopCtx>,
    /// Whether a `redo` compiled into the current chunk has something that will
    /// re-run it. True inside a block/lambda body (`call_proc` re-runs it) and
    /// inside a `begin` nested in a native loop (the loop's `TAKE_LOOP_REDO`
    /// handoff re-runs it). `self.loops` covers the direct in-loop case on its
    /// own, but it is emptied when compiling a nested chunk, so this flag carries
    /// the fact across that boundary. Anywhere else MRI raises
    /// `Invalid redo (SyntaxError)`, and so does this compiler. Reset (and
    /// restored) across a method body, which is a new `redo` context.
    redo_ok: bool,
    /// Monotonic counter for unique temporaries (`case/in` subject slots).
    tmp: usize,
    /// When true (`--dap`), emit a per-statement `Op::Extended(DBG_LINE)` marker
    /// carrying the source line so the debugger can pause at statement
    /// boundaries. Off for normal runs — zero extra ops in the chunk.
    debug: bool,
    /// The lexical namespace stack (outermost first) as fully-qualified names:
    /// inside `module A; module B` it is `["A", "A::B"]`. Constants defined in a
    /// namespace are stored under `<innermost>::<name>`; a bare constant read is
    /// compiled to a candidate chain that walks this nesting then the top level
    /// (Ruby's lexical constant lookup). Superclass/include names are resolved
    /// against it too.
    nesting: Vec<String>,
    /// The source line of the statement currently being compiled. Baked into the
    /// call/read ops that can raise (`CALL*`, `YIELD`, `SUPER*`, var reads,
    /// index) so `abort` can report an MRI-format backtrace line
    /// (`<src>:<line>:in '<ctx>'`) for an uncaught exception. Saved/restored
    /// around nested body compiles so an inner block/def can't leak its line.
    cur_line: u32,
    /// Per-scope set of names that are LOCAL VARIABLES (a parameter, or assigned
    /// somewhere in the method/block). A bare lowercase identifier compiles to a
    /// local read only when it is in this set; otherwise it is a zero-arg method
    /// call — Ruby's rule, and what lets `countdown`/`parse_expressions` recurse
    /// by bareword (a same-named-as-method local would shadow, but a never-
    /// assigned name is unambiguously a call). Innermost scope is last; a block
    /// inherits the enclosing scope's locals (closures) plus its own.
    scope_locals: Vec<std::collections::HashSet<String>>,
    /// The name of the method currently being compiled (a stack for nested
    /// `def`s). A bare identifier equal to this — and not shadowed by a local —
    /// is a self-recursive call, not a nil local read.
    cur_method: Vec<String>,
    /// Per-scope slot lowering, parallel in depth to `scope_locals`. A name in
    /// the top map is a fusevm frame slot (`GetSlot`/`SetSlot`) instead of a host
    /// `GETLOCAL`/`SETLOCAL` dispatch — which is what lets fusevm's AOT/JIT native
    /// path lower the surrounding arithmetic to registers (a host `CallBuiltin`
    /// is opaque and forces the interpreter). Only locals proven **not** captured
    /// by a nested closure and **not** introspected (`binding`/`eval`/`defined?`/
    /// `local_variables`) get a slot; every other name keeps the host path so
    /// closure sharing and reflection stay correct. Empty top map = no slot
    /// lowering in this scope (a closure/class body, or an introspecting scope).
    slot_scopes: Vec<std::collections::HashMap<String, u16>>,
}

/// Compile a parsed program. `debug` enables per-statement DAP line markers.
/// Process-global counter for synthetic method names (`__class_body__N`,
/// `__def N__`). These must be unique across *all* compilation units, not just
/// within one file: a per-compiler counter (reset to 0 each `compile()`) makes
/// two files that reopen the same class both emit `__class_body__0`, and merging
/// their `ClassDef`s clobbers one class body with the other — so a reopened
/// class's init-body silently never runs (this broke i18n/activesupport, whose
/// `module I18n` is reopened across version.rb/utils.rb/i18n.rb). The names are
/// only ever referenced within the same Program's own bytecode (call site uses
/// the exact name it minted), so a global counter stays self-consistent and
/// cache-safe.
fn next_synth_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SYNTH_CTR: AtomicU64 = AtomicU64::new(0);
    SYNTH_CTR.fetch_add(1, Ordering::Relaxed)
}

// ────────────────────────────────────────────────────────────────────────────
// Slot lowering analysis (see `Compiler::slot_scopes`)
//
// A local is lowered to a fusevm frame slot (native-lowerable) instead of a host
// `GETLOCAL`/`SETLOCAL` dispatch only when it is provably safe: not shared with a
// closure, not reached by reflection, and written only by plain assignment. The
// analysis is deliberately conservative — any construct it can't reason about
// disqualifies the whole scope, so a missed case costs speed, never correctness.
// ────────────────────────────────────────────────────────────────────────────

/// Bare-call names that can read or mutate a scope's locals by reflection, so any
/// of them anywhere in the scope tree (including nested closures — a captured
/// `binding` reaches outer locals) forbids slot lowering.
const INTROSPECT_CALLS: &[&str] = &[
    "eval",
    "binding",
    "local_variables",
    "instance_eval",
    "instance_exec",
    "class_eval",
    "module_eval",
    "class_exec",
    "module_exec",
];

/// Walk a scope body collecting (a) local names referenced inside a nested
/// closure — which must stay host-bound so the closure and its parent share one
/// binding — and (b) a "disqualified" flag set when the scope uses reflection or
/// an implicit/complex binding form (`for`, `case/in`, multiple assignment,
/// `rescue => e`, `defined?` on a local) that slot lowering can't track. A nested
/// `def`/`class`/`module` is a *separate* scope (no shared locals), so the walk
/// stops at it.
fn slot_scan(body: &[Stmt]) -> (std::collections::HashSet<String>, bool) {
    let mut caps = std::collections::HashSet::new();
    let mut disq = false;
    for s in body {
        slot_scan_expr(&s.expr, &mut caps, &mut disq, false);
    }
    (caps, disq)
}

fn slot_scan_block(b: &Block, caps: &mut std::collections::HashSet<String>, disq: &mut bool) {
    // A block body is a closure: every local it names is shared with the parent.
    for s in &b.body {
        slot_scan_expr(&s.expr, caps, disq, true);
    }
}

fn slot_scan_body(
    body: &[Stmt],
    caps: &mut std::collections::HashSet<String>,
    disq: &mut bool,
    in_closure: bool,
) {
    for s in body {
        slot_scan_expr(&s.expr, caps, disq, in_closure);
    }
}

fn slot_scan_expr(
    e: &Expr,
    caps: &mut std::collections::HashSet<String>,
    disq: &mut bool,
    in_closure: bool,
) {
    match e {
        Expr::Var(VarKind::Local, n) => {
            if in_closure {
                caps.insert(n.clone());
            }
        }
        Expr::Nil
        | Expr::True
        | Expr::False
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::Symbol(_)
        | Expr::SelfExpr
        | Expr::Regex(_, _)
        | Expr::Retry
        | Expr::Redo
        | Expr::Var(_, _) => {}
        Expr::Str(parts) => {
            for p in parts {
                if let StrPart::Interp(x) = p {
                    slot_scan_expr(x, caps, disq, in_closure);
                }
            }
        }
        Expr::Array(xs) => {
            for x in xs {
                slot_scan_expr(x, caps, disq, in_closure);
            }
        }
        Expr::Hash(kvs) => {
            for (k, v) in kvs {
                slot_scan_expr(k, caps, disq, in_closure);
                slot_scan_expr(v, caps, disq, in_closure);
            }
        }
        Expr::Range { lo, hi, .. } => {
            if let Some(x) = lo {
                slot_scan_expr(x, caps, disq, in_closure);
            }
            if let Some(x) = hi {
                slot_scan_expr(x, caps, disq, in_closure);
            }
        }
        Expr::Assign(t, v) => {
            slot_scan_expr(t, caps, disq, in_closure);
            slot_scan_expr(v, caps, disq, in_closure);
        }
        Expr::MultiAssign { targets, values } => {
            if !in_closure {
                *disq = true;
            }
            for t in targets {
                slot_scan_expr(t, caps, disq, in_closure);
            }
            for v in values {
                slot_scan_expr(v, caps, disq, in_closure);
            }
        }
        Expr::Unary(_, x) => slot_scan_expr(x, caps, disq, in_closure),
        Expr::Binary(_, a, b) => {
            slot_scan_expr(a, caps, disq, in_closure);
            slot_scan_expr(b, caps, disq, in_closure);
        }
        Expr::If {
            cond,
            then,
            elifs,
            els,
        } => {
            slot_scan_expr(cond, caps, disq, in_closure);
            slot_scan_body(then, caps, disq, in_closure);
            for (c, b) in elifs {
                slot_scan_expr(c, caps, disq, in_closure);
                slot_scan_body(b, caps, disq, in_closure);
            }
            if let Some(b) = els {
                slot_scan_body(b, caps, disq, in_closure);
            }
        }
        Expr::While { cond, body } | Expr::DoWhile { cond, body } => {
            slot_scan_expr(cond, caps, disq, in_closure);
            slot_scan_body(body, caps, disq, in_closure);
        }
        Expr::For { iter, body, .. } => {
            // `for v in …` binds `v` in the enclosing scope with no assignment
            // node the plan can see — disqualify rather than risk an unwritten
            // slot.
            if !in_closure {
                *disq = true;
            }
            slot_scan_expr(iter, caps, disq, in_closure);
            slot_scan_body(body, caps, disq, in_closure);
        }
        Expr::Case {
            subject,
            whens,
            els,
        } => {
            slot_scan_expr(subject, caps, disq, in_closure);
            for (pats, b) in whens {
                for p in pats {
                    slot_scan_expr(p, caps, disq, in_closure);
                }
                slot_scan_body(b, caps, disq, in_closure);
            }
            if let Some(b) = els {
                slot_scan_body(b, caps, disq, in_closure);
            }
        }
        Expr::CaseIn { subject, els, .. } => {
            // Pattern matching binds locals structurally — disqualify.
            if !in_closure {
                *disq = true;
            }
            slot_scan_expr(subject, caps, disq, in_closure);
            if let Some(b) = els {
                slot_scan_body(b, caps, disq, in_closure);
            }
        }
        Expr::Call {
            recv,
            name,
            args,
            block,
        } => {
            if INTROSPECT_CALLS.contains(&name.as_str()) {
                *disq = true;
            }
            if let Some(r) = recv {
                slot_scan_expr(r, caps, disq, in_closure);
            }
            for a in args {
                slot_scan_expr(a, caps, disq, in_closure);
            }
            if let Some(b) = block {
                slot_scan_block(b, caps, disq);
            }
        }
        Expr::Index(r, idxs) => {
            slot_scan_expr(r, caps, disq, in_closure);
            for i in idxs {
                slot_scan_expr(i, caps, disq, in_closure);
            }
        }
        Expr::BlockPass(x) | Expr::Splat(x) => slot_scan_expr(x, caps, disq, in_closure),
        // Separate scopes: a nested def/class/module/singleton-class body cannot
        // reference this scope's locals, so it neither captures nor disqualifies.
        Expr::Def { .. }
        | Expr::Class { .. }
        | Expr::Module { .. }
        | Expr::SingletonClass { .. } => {}
        Expr::Begin {
            body,
            rescues,
            ensure,
        } => {
            // `begin`/`rescue`/`ensure` bodies each compile to their own proc
            // frame (`compile_begin`), so from this scope they are closures: any
            // local they touch is shared cross-frame and must stay host-bound.
            slot_scan_body(body, caps, disq, true);
            for r in rescues {
                if let Some(s) = &r.splat {
                    slot_scan_expr(s, caps, disq, true);
                }
                slot_scan_body(&r.body, caps, disq, true);
            }
            if let Some(en) = ensure {
                slot_scan_body(en, caps, disq, true);
            }
        }
        Expr::Return(o) | Expr::Break(o) | Expr::Next(o) => {
            if let Some(x) = o {
                slot_scan_expr(x, caps, disq, in_closure);
            }
        }
        Expr::Yield(xs) => {
            for x in xs {
                slot_scan_expr(x, caps, disq, in_closure);
            }
        }
        Expr::Defined(inner) => {
            if let Expr::Var(VarKind::Local, _) = &**inner {
                *disq = true;
            }
            slot_scan_expr(inner, caps, disq, in_closure);
        }
        Expr::Super { args, block } => {
            if let Some(a) = args {
                for x in a {
                    slot_scan_expr(x, caps, disq, in_closure);
                }
            }
            if let Some(b) = block {
                slot_scan_block(b, caps, disq);
            }
        }
        Expr::Lambda(b) => slot_scan_block(b, caps, disq),
    }
}

/// Collect, in first-seen order, every local written by a plain
/// `Assign(Var(Local), …)` at this scope's own level (not inside a nested closure
/// or def). In a non-disqualified scope these are the *only* local writes, so the
/// set is exactly the slot candidates before removing params/captures.
fn scope_plain_assigns(body: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for s in body {
        collect_plain_assigns(&s.expr, &mut out);
    }
    out
}

/// Every local a `for` body assigns, in first-seen order.
///
/// Ruby's `for` introduces no scope, so a local first assigned in the body
/// outlives the loop exactly like the loop variable does
/// (`for i in 1..3; sq = i * i; end` leaves both `i` and `sq` behind, and
/// `defined?(sq)` is `"local-variable"` even when the loop body never ran).
/// The body is lowered to an `each` block, whose locals *are* block-local, so
/// `compile_for` pre-declares every name this returns in the enclosing scope —
/// the block's assignment then writes through to that binding instead of making
/// a fresh one per iteration (which is also what makes a closure created in the
/// body share the final value, as MRI does).
///
/// The walk follows control flow — `if`/`while`/`case`/`case-in`/`begin` and a
/// nested `for`, all of which are the same scope in MRI — and stops at the
/// things that genuinely open a new one: a block or lambda body, a `def`, and a
/// `class`/`module` body.
fn for_body_locals(body: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for s in body {
        for_locals_expr(&s.expr, &mut out);
    }
    out
}

fn push_local(name: &str, out: &mut Vec<String>) {
    if !out.iter().any(|x| x == name) {
        out.push(name.to_string());
    }
}

/// Record the locals an assignment target binds (`a`, `*rest`, or a nested
/// destructuring target list).
fn for_locals_target(t: &Expr, out: &mut Vec<String>) {
    match t {
        Expr::Var(VarKind::Local, n) => push_local(n, out),
        Expr::Splat(inner) => for_locals_target(inner, out),
        Expr::Array(items) => items.iter().for_each(|i| for_locals_target(i, out)),
        _ => {}
    }
}

fn for_locals_pattern(p: &Pattern, out: &mut Vec<String>) {
    match p {
        Pattern::Bind(n) => push_local(n, out),
        Pattern::Splat(Some(n)) => push_local(n, out),
        Pattern::As(inner, n) => {
            for_locals_pattern(inner, out);
            push_local(n, out);
        }
        Pattern::Array(ps) | Pattern::Or(ps) => ps.iter().for_each(|p| for_locals_pattern(p, out)),
        Pattern::Hash(fields, _) => {
            for (key, sub) in fields {
                match sub {
                    // `{key:}` shorthand binds `key` itself.
                    None => push_local(key, out),
                    Some(p) => for_locals_pattern(p, out),
                }
            }
        }
        Pattern::Const(_, Some(inner)) => for_locals_pattern(inner, out),
        _ => {}
    }
}

fn for_locals_body(body: &[Stmt], out: &mut Vec<String>) {
    for s in body {
        for_locals_expr(&s.expr, out);
    }
}

fn for_locals_expr(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Assign(t, v) => {
            for_locals_target(t, out);
            for_locals_expr(v, out);
        }
        Expr::MultiAssign { targets, values } => {
            targets.iter().for_each(|t| for_locals_target(t, out));
            values.iter().for_each(|v| for_locals_expr(v, out));
        }
        Expr::If {
            cond,
            then,
            elifs,
            els,
        } => {
            for_locals_expr(cond, out);
            for_locals_body(then, out);
            for (c, b) in elifs {
                for_locals_expr(c, out);
                for_locals_body(b, out);
            }
            if let Some(b) = els {
                for_locals_body(b, out);
            }
        }
        Expr::While { cond, body } | Expr::DoWhile { cond, body } => {
            for_locals_expr(cond, out);
            for_locals_body(body, out);
        }
        // A nested `for` leaks its own loop variable and body locals too.
        Expr::For { var, iter, body } => {
            push_local(var, out);
            for_locals_expr(iter, out);
            for_locals_body(body, out);
        }
        Expr::Case {
            subject,
            whens,
            els,
        } => {
            for_locals_expr(subject, out);
            for (tests, b) in whens {
                tests.iter().for_each(|t| for_locals_expr(t, out));
                for_locals_body(b, out);
            }
            if let Some(b) = els {
                for_locals_body(b, out);
            }
        }
        Expr::CaseIn {
            subject,
            clauses,
            els,
        } => {
            for_locals_expr(subject, out);
            for c in clauses {
                for_locals_pattern(&c.pattern, out);
                if let Some(g) = &c.guard {
                    for_locals_expr(g, out);
                }
                for_locals_body(&c.body, out);
            }
            if let Some(b) = els {
                for_locals_body(b, out);
            }
        }
        Expr::Begin {
            body,
            rescues,
            ensure,
        } => {
            for_locals_body(body, out);
            for r in rescues {
                // `rescue => e` binds `e` in the enclosing scope.
                if let Some(n) = &r.binding {
                    push_local(n, out);
                }
                for_locals_body(&r.body, out);
            }
            if let Some(b) = ensure {
                for_locals_body(b, out);
            }
        }
        // A block/lambda body is its own scope in MRI too — do not descend into
        // it, but the receiver and arguments of the call are still this scope.
        Expr::Call { recv, args, .. } => {
            if let Some(r) = recv {
                for_locals_expr(r, out);
            }
            args.iter().for_each(|a| for_locals_expr(a, out));
        }
        Expr::Binary(_, l, r) => {
            for_locals_expr(l, out);
            for_locals_expr(r, out);
        }
        Expr::Unary(_, x) | Expr::Splat(x) | Expr::BlockPass(x) | Expr::Defined(x) => {
            for_locals_expr(x, out)
        }
        Expr::Index(r, idx) => {
            for_locals_expr(r, out);
            idx.iter().for_each(|i| for_locals_expr(i, out));
        }
        Expr::Array(xs) | Expr::Yield(xs) => xs.iter().for_each(|x| for_locals_expr(x, out)),
        Expr::Hash(pairs) => {
            for (k, v) in pairs {
                for_locals_expr(k, out);
                for_locals_expr(v, out);
            }
        }
        Expr::Range { lo, hi, .. } => {
            if let Some(x) = lo {
                for_locals_expr(x, out);
            }
            if let Some(x) = hi {
                for_locals_expr(x, out);
            }
        }
        Expr::Str(parts) => {
            for p in parts {
                if let StrPart::Interp(x) = p {
                    for_locals_expr(x, out);
                }
            }
        }
        Expr::Return(o) | Expr::Break(o) | Expr::Next(o) => {
            if let Some(x) = o {
                for_locals_expr(x, out);
            }
        }
        Expr::Super { args: Some(a), .. } => a.iter().for_each(|x| for_locals_expr(x, out)),
        _ => {}
    }
}

fn collect_plain_assigns(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Assign(t, v) => {
            if let Expr::Var(VarKind::Local, n) = &**t {
                if !out.iter().any(|x| x == n) {
                    out.push(n.clone());
                }
            }
            collect_plain_assigns(v, out);
        }
        Expr::Nil
        | Expr::True
        | Expr::False
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::Symbol(_)
        | Expr::SelfExpr
        | Expr::Regex(_, _)
        | Expr::Retry
        | Expr::Redo
        | Expr::Var(_, _) => {}
        Expr::Str(parts) => {
            for p in parts {
                if let StrPart::Interp(x) = p {
                    collect_plain_assigns(x, out);
                }
            }
        }
        Expr::Array(xs) => xs.iter().for_each(|x| collect_plain_assigns(x, out)),
        Expr::Hash(kvs) => kvs.iter().for_each(|(k, v)| {
            collect_plain_assigns(k, out);
            collect_plain_assigns(v, out);
        }),
        Expr::Range { lo, hi, .. } => {
            if let Some(x) = lo {
                collect_plain_assigns(x, out);
            }
            if let Some(x) = hi {
                collect_plain_assigns(x, out);
            }
        }
        Expr::MultiAssign { values, .. } => {
            values.iter().for_each(|v| collect_plain_assigns(v, out))
        }
        Expr::Unary(_, x) | Expr::BlockPass(x) | Expr::Splat(x) => collect_plain_assigns(x, out),
        Expr::Binary(_, a, b) => {
            collect_plain_assigns(a, out);
            collect_plain_assigns(b, out);
        }
        Expr::If {
            cond,
            then,
            elifs,
            els,
        } => {
            collect_plain_assigns(cond, out);
            then.iter()
                .for_each(|s| collect_plain_assigns(&s.expr, out));
            for (c, b) in elifs {
                collect_plain_assigns(c, out);
                b.iter().for_each(|s| collect_plain_assigns(&s.expr, out));
            }
            if let Some(b) = els {
                b.iter().for_each(|s| collect_plain_assigns(&s.expr, out));
            }
        }
        Expr::While { cond, body } | Expr::DoWhile { cond, body } => {
            collect_plain_assigns(cond, out);
            body.iter()
                .for_each(|s| collect_plain_assigns(&s.expr, out));
        }
        Expr::For { iter, body, .. } => {
            collect_plain_assigns(iter, out);
            body.iter()
                .for_each(|s| collect_plain_assigns(&s.expr, out));
        }
        Expr::Case {
            subject,
            whens,
            els,
        } => {
            collect_plain_assigns(subject, out);
            for (pats, b) in whens {
                pats.iter().for_each(|p| collect_plain_assigns(p, out));
                b.iter().for_each(|s| collect_plain_assigns(&s.expr, out));
            }
            if let Some(b) = els {
                b.iter().for_each(|s| collect_plain_assigns(&s.expr, out));
            }
        }
        Expr::CaseIn { subject, els, .. } => {
            collect_plain_assigns(subject, out);
            if let Some(b) = els {
                b.iter().for_each(|s| collect_plain_assigns(&s.expr, out));
            }
        }
        Expr::Call { recv, args, .. } => {
            if let Some(r) = recv {
                collect_plain_assigns(r, out);
            }
            args.iter().for_each(|a| collect_plain_assigns(a, out));
        }
        Expr::Index(r, idxs) => {
            collect_plain_assigns(r, out);
            idxs.iter().for_each(|i| collect_plain_assigns(i, out));
        }
        // Separate scopes / closures: their assignments are not this scope's.
        // `begin`/`rescue`/`ensure` each compile to their own proc frame, so —
        // like a def/class/lambda — their internal assignments belong to that
        // frame, not this one, and must not become slot candidates here.
        Expr::Def { .. }
        | Expr::Class { .. }
        | Expr::Module { .. }
        | Expr::SingletonClass { .. }
        | Expr::Lambda(_)
        | Expr::Begin { .. } => {}
        Expr::Return(o) | Expr::Break(o) | Expr::Next(o) => {
            if let Some(x) = o {
                collect_plain_assigns(x, out);
            }
        }
        Expr::Yield(xs) => xs.iter().for_each(|x| collect_plain_assigns(x, out)),
        Expr::Defined(inner) => collect_plain_assigns(inner, out),
        Expr::Super { args, .. } => {
            if let Some(a) = args {
                a.iter().for_each(|x| collect_plain_assigns(x, out));
            }
        }
    }
}

pub fn compile(stmts: &[Stmt], debug: bool) -> Result<Program, String> {
    let mut c = Compiler {
        debug,
        ..Default::default()
    };
    let mut b = ChunkBuilder::new();
    // The top-level script is a scope with no parameters; slot-lower its locals.
    c.enter_slot_scope(&std::collections::HashSet::new(), stmts);
    c.compile_seq(&mut b, stmts)?;
    c.exit_slot_scope();
    Ok(Program {
        main: b.build(),
        methods: c.methods,
        classes: c.classes,
        begins: c.begins,
        procs: c.procs,
    })
}

/// Rewrite every proc-id and begin-id reference in `prog` so its ids sit above
/// the ids already loaded on the host (`proc_off`/`begin_off` = the host's
/// current `procs.len()`/`begins.len()`). Without this, a second file's ids
/// start at 0 and alias the first file's already-loaded procs/begins, so the
/// wrong block/begin body would run at dispatch time. Proc ids appear as the
/// `Op::LoadInt` immediately before a `CallBuiltin(MKPROC|MKLAMBDA, 1)`, begin
/// ids as the `LoadInt` before `CallBuiltin(BEGIN, 1)`; a `BeginDef`'s
/// `body`/`ensure`/rescue-`body` fields hold proc ids directly (not in a chunk).
pub fn rebase_program(prog: &mut Program, proc_off: usize, begin_off: usize) {
    if proc_off == 0 && begin_off == 0 {
        return;
    }
    rebase_chunk(&mut prog.main, proc_off, begin_off);
    for (_, m) in &mut prog.methods {
        rebase_chunk(&mut m.chunk, proc_off, begin_off);
    }
    for (_, c) in &mut prog.classes {
        for (_, m) in &mut c.methods {
            rebase_chunk(&mut m.chunk, proc_off, begin_off);
        }
        for (_, m) in &mut c.class_methods {
            rebase_chunk(&mut m.chunk, proc_off, begin_off);
        }
    }
    for p in &mut prog.procs {
        rebase_chunk(&mut p.chunk, proc_off, begin_off);
    }
    for bd in &mut prog.begins {
        bd.body += proc_off;
        if let Some(e) = &mut bd.ensure {
            *e += proc_off;
        }
        for r in &mut bd.rescues {
            r.body += proc_off;
            if let Some(s) = &mut r.splat {
                *s += proc_off;
            }
        }
    }
}

/// Add the offsets to the proc/begin id operands inside one chunk (and, for
/// completeness, any fusevm sub-chunks — rubylang never emits them, but the
/// recursion keeps this correct if that ever changes). The proc/begin id is
/// always the `Op::LoadInt` immediately preceding the consuming builtin, by
/// construction of the compiler.
fn rebase_chunk(chunk: &mut Chunk, proc_off: usize, begin_off: usize) {
    let mut changed = false;
    for i in 1..chunk.ops.len() {
        if let Op::CallBuiltin(id, 1) = chunk.ops[i] {
            let off = if id == ops::MKPROC || id == ops::MKLAMBDA {
                proc_off
            } else if id == ops::BEGIN {
                begin_off
            } else {
                continue;
            };
            if off == 0 {
                continue;
            }
            if let Op::LoadInt(v) = &mut chunk.ops[i - 1] {
                *v += off as i64;
                changed = true;
            }
        }
    }
    for sub in &mut chunk.sub_chunks {
        rebase_chunk(sub, proc_off, begin_off);
    }
    if changed {
        // The op hash is the JIT-cache key (`#[serde(skip)]`, computed at build
        // time from ops + constants). Recompute it so the rewritten chunk keys
        // to its actual ops and never collides with a different chunk.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        chunk.ops.hash(&mut h);
        chunk.constants.hash(&mut h);
        chunk.op_hash = h.finish();
    }
}

/// Compile a program whose `proc`/`begin` template ids start at the given bases,
/// returning only the newly-emitted procs/begins. Used by `eval` so a string
/// compiled into an already-running host references the host's proc/begin tables
/// at the correct absolute offsets when its new templates are appended.
pub fn compile_at(stmts: &[Stmt], proc_base: usize, begin_base: usize) -> Result<Program, String> {
    // Seed placeholder templates so freshly-assigned ids start past the bases;
    // they are dropped from the returned program (only the new tail is kept).
    let mut c = Compiler {
        procs: (0..proc_base).map(|_| dummy_proc()).collect(),
        begins: (0..begin_base)
            .map(|_| BeginDef {
                body: 0,
                rescues: vec![],
                ensure: None,
            })
            .collect(),
        ..Default::default()
    };
    let mut b = ChunkBuilder::new();
    // `eval` shares the caller's binding, so its locals must stay host-bound; push
    // an empty (inactive) slot scope so nothing here is slot-lowered.
    c.slot_scopes.push(std::collections::HashMap::new());
    c.compile_seq(&mut b, stmts)?;
    c.slot_scopes.pop();
    Ok(Program {
        main: b.build(),
        methods: c.methods,
        classes: c.classes,
        begins: c.begins.split_off(begin_base),
        procs: c.procs.split_off(proc_base),
    })
}

fn dummy_proc() -> ProcDef {
    ProcDef {
        params: vec![],
        splat: None,
        chunk: ChunkBuilder::new().build(),
        arity: crate::ast::BlockArity::default(),
    }
}

impl Compiler {
    /// Compile a sequence of statements as a body: each value but the last is
    /// popped; the last is left on the stack (Ruby's "value is the last expr").
    /// In debug mode each statement is preceded by a line marker.
    fn compile_seq(&mut self, b: &mut ChunkBuilder, body: &[Stmt]) -> Result<(), String> {
        if body.is_empty() {
            b.emit(Op::LoadUndef, 0);
            return Ok(());
        }
        for (i, s) in body.iter().enumerate() {
            if s.line != 0 {
                self.cur_line = s.line;
            }
            if self.debug && s.line != 0 {
                b.emit(Op::Extended(crate::host::ext::DBG_LINE, 0), s.line);
            }
            // Tag a compile error with the statement's source line so loader walls
            // in deep gem trees are locatable (`line N: <error>`).
            self.compile_expr(b, &s.expr).map_err(|e| {
                if s.line != 0 && !e.starts_with("line ") {
                    format!("line {}: {e}", s.line)
                } else {
                    e
                }
            })?;
            if i + 1 < body.len() {
                b.emit(Op::Pop, 0);
            }
        }
        Ok(())
    }

    /// Compile a `def` body and register it under a fresh synthetic name, so a
    /// runtime `DEFINE_SINGLETON`/`DEFINE_METHOD_DYN` op can fetch the compiled
    /// `MethodDef` from the host by that name. Returns the synthetic name.
    fn stash_method(&mut self, params: &[Param], body: &[Stmt]) -> Result<String, String> {
        let def = self.compile_method(params, body)?;
        let synth = format!("__def{}__", next_synth_id());
        self.methods.push((synth.clone(), def));
        Ok(synth)
    }

    /// Compile a method: a prologue that fills in any defaulted parameter the
    /// caller omitted (`defined?(p) ? p : <default>`), then the body.
    fn compile_method(&mut self, params: &[Param], body: &[Stmt]) -> Result<MethodDef, String> {
        self.compile_method_named("", params, body)
    }

    /// Collect the `VarKind::Local` names assigned anywhere inside a parameter
    /// default expression (e.g. `no_default` in `default = (no_default = true)`),
    /// so they can be hoisted to nil at method entry — Ruby scopes them as method
    /// locals from their parse position regardless of whether the default runs.
    fn collect_default_locals(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Assign(target, value) => {
                if let Expr::Var(VarKind::Local, n) = &**target {
                    if !out.contains(n) {
                        out.push(n.clone());
                    }
                }
                Self::collect_default_locals(value, out);
            }
            Expr::MultiAssign { targets, values } => {
                for t in targets {
                    if let Expr::Var(VarKind::Local, n) = t {
                        if !out.contains(n) {
                            out.push(n.clone());
                        }
                    }
                }
                for v in values {
                    Self::collect_default_locals(v, out);
                }
            }
            Expr::Binary(_, l, r) => {
                Self::collect_default_locals(l, out);
                Self::collect_default_locals(r, out);
            }
            Expr::Unary(_, x) | Expr::Splat(x) => Self::collect_default_locals(x, out),
            Expr::Index(r, idx) => {
                Self::collect_default_locals(r, out);
                for i in idx {
                    Self::collect_default_locals(i, out);
                }
            }
            Expr::Array(items) => {
                for i in items {
                    Self::collect_default_locals(i, out);
                }
            }
            Expr::Call { recv, args, .. } => {
                if let Some(r) = recv {
                    Self::collect_default_locals(r, out);
                }
                for a in args {
                    Self::collect_default_locals(a, out);
                }
            }
            _ => {}
        }
    }
    fn compile_method_named(
        &mut self,
        name: &str,
        params: &[Param],
        body: &[Stmt],
    ) -> Result<MethodDef, String> {
        let saved = std::mem::take(&mut self.loops);
        let saved_redo = std::mem::take(&mut self.redo_ok);
        let saved_line = self.cur_line;
        // A fresh local scope: parameters are its initial locals.
        let param_locals: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        self.scope_locals.push(param_locals);
        self.cur_method.push(name.to_string());
        let mut b = ChunkBuilder::new();
        // Ruby hoists a local to nil from its first assignment position, even when
        // that assignment lives inside a *parameter default* that may not run.
        // `def index_with(default = (no_default = true))` introduces `no_default`
        // as a method local; when a `default` arg is passed the assignment is
        // skipped, yet a later bare read of `no_default` must see nil (a local),
        // not dispatch as a method. Pre-bind every such local to nil so a skipped
        // default still leaves it declared (a run default overwrites it).
        let mut default_locals: Vec<String> = Vec::new();
        for p in params {
            if let Some(default) = &p.default {
                Self::collect_default_locals(default, &mut default_locals);
            }
        }
        // Slot-lower the body's locals, binding simple positional params directly
        // into leading frame slots when it is safe (the dispatcher seeds them from
        // the call args). `n_slot_params` rides into the `MethodDef` so dispatch
        // knows how many args to place. A non-simple signature keeps every param
        // host-bound (the prologue's `SETLOCAL`s), slot-lowering only body locals.
        let n_slot_params = self.enter_method_slot_scope(params, &default_locals, body);
        for lname in &default_locals {
            if params.iter().any(|p| &p.name == lname) {
                continue;
            }
            if let Some(scope) = self.scope_locals.last_mut() {
                scope.insert(lname.clone());
            }
            self.kstr(&mut b, lname);
            self.compile_expr(&mut b, &Expr::Nil)?;
            b.emit(Op::CallBuiltin(ops::SETLOCAL, 2), 0);
            b.emit(Op::Pop, 0);
        }
        for p in params {
            if let Some(default) = &p.default {
                // if !defined?(p) { p = default }
                self.kstr(&mut b, &p.name);
                b.emit(Op::CallBuiltin(ops::DEFINED, 1), 0);
                let skip = b.emit(Op::JumpIfTrue(0), 0);
                self.compile_assign(&mut b, &Expr::Var(VarKind::Local, p.name.clone()), default)?;
                b.emit(Op::Pop, 0);
                let here = b.current_pos();
                b.patch_jump(skip, here);
            }
        }
        self.compile_seq(&mut b, body)?;
        self.exit_slot_scope();
        self.scope_locals.pop();
        self.cur_method.pop();
        self.loops = saved;
        self.redo_ok = saved_redo;
        self.cur_line = saved_line;
        // Positional params (name-bound by index) are separate from keyword
        // params (bound from the trailing keyword hash).
        let pnames: Vec<String> = params
            .iter()
            .filter(|p| !p.keyword && !p.kwsplat && !p.block)
            .map(|p| p.name.clone())
            .collect();
        let blockparam = params.iter().find(|p| p.block).map(|p| p.name.clone());
        let kwparams: Vec<String> = params
            .iter()
            .filter(|p| p.keyword)
            .map(|p| p.name.clone())
            .collect();
        let kwsplat = params.iter().find(|p| p.kwsplat).map(|p| p.name.clone());
        let splat = params
            .iter()
            .filter(|p| !p.keyword && !p.kwsplat && !p.block)
            .position(|p| p.splat);
        // Arity, for the pre-body `ArgumentError`. A `*splat` is neither
        // required nor optional (it absorbs any surplus), so it counts as
        // neither; every other positional param is one or the other.
        let req = params
            .iter()
            .filter(|p| !p.keyword && !p.kwsplat && !p.block && !p.splat && p.default.is_none())
            .count() as u16;
        let opt = params
            .iter()
            .filter(|p| !p.keyword && !p.kwsplat && !p.block && !p.splat && p.default.is_some())
            .count() as u16;
        let kwreq: Vec<String> = params
            .iter()
            .filter(|p| p.keyword && p.default.is_none())
            .map(|p| p.name.clone())
            .collect();
        Ok(MethodDef {
            params: pnames,
            splat,
            kwparams,
            kwsplat,
            blockparam,
            req,
            opt,
            kwreq,
            chunk: b.build(),
            slot_params: n_slot_params,
        })
    }

    fn compile_body_chunk(&mut self, body: &[Stmt]) -> Result<Chunk, String> {
        // A method/proc body is its own chunk; a native-loop context from the
        // enclosing chunk does not cross into it (break/next become signals).
        let saved = std::mem::take(&mut self.loops);
        let saved_line = self.cur_line;
        let mut b = ChunkBuilder::new();
        // A proc/block body runs in its own fusevm frame, so the enclosing scope's
        // slot indices don't apply here. Push an empty (inactive) slot scope: block
        // locals stay host-bound (shared with the enclosing binding, which is why
        // the capture analysis already forces any block-referenced local host).
        self.slot_scopes.push(std::collections::HashMap::new());
        self.compile_seq(&mut b, body)?;
        self.slot_scopes.pop();
        self.loops = saved;
        self.cur_line = saved_line;
        Ok(b.build())
    }

    fn compile_expr(&mut self, b: &mut ChunkBuilder, e: &Expr) -> Result<(), String> {
        // `__LINE__` is a keyword the compiler resolves to the current source
        // line — MRI substitutes the integer literal, it is never a runtime
        // call. Depending on context it parses as a bare call or an unassigned
        // local; handle both. Ubiquitous in the `class_eval <<-CODE, __FILE__,
        // __LINE__ + 1` gem idiom (i18n, thor, activesupport all use it).
        match e {
            Expr::Call {
                recv: None,
                name,
                args,
                ..
            } if name == "__LINE__" && args.is_empty() => {
                b.emit(Op::LoadInt(self.cur_line as i64), 0);
                return Ok(());
            }
            Expr::Var(VarKind::Local, name) if name == "__LINE__" => {
                b.emit(Op::LoadInt(self.cur_line as i64), 0);
                return Ok(());
            }
            _ => {}
        }
        match e {
            Expr::Nil => {
                b.emit(Op::LoadUndef, 0);
            }
            Expr::True => {
                b.emit(Op::LoadTrue, 0);
            }
            Expr::False => {
                b.emit(Op::LoadFalse, 0);
            }
            Expr::Int(n) => {
                b.emit(Op::LoadInt(*n), 0);
            }
            Expr::Float(f) => {
                b.emit(Op::LoadFloat(*f), 0);
            }
            Expr::Str(parts) => {
                for p in parts {
                    match p {
                        StrPart::Lit(s) => self.kstr(b, s),
                        StrPart::Interp(e) => self.compile_expr(b, e)?,
                    }
                }
                // Under `# frozen_string_literal: true`, a *non-interpolated*
                // literal is frozen (MRI leaves interpolated strings mutable).
                let all_literal = parts.iter().all(|p| matches!(p, StrPart::Lit(_)));
                let op = if frozen_string_literals() && all_literal {
                    ops::MKSTRF
                } else {
                    ops::MKSTR
                };
                b.emit(Op::CallBuiltin(op, parts.len() as u8), 0);
            }
            Expr::Symbol(s) => {
                self.kstr(b, s);
                b.emit(Op::CallBuiltin(ops::MKSYM, 1), 0);
            }
            Expr::Array(items) => {
                if items.iter().any(|it| matches!(it, Expr::Splat(_))) {
                    self.compile_spread(b, items)?;
                } else if items.len() <= 255 {
                    for it in items {
                        self.compile_expr(b, it)?;
                    }
                    b.emit(Op::CallBuiltin(ops::MKARRAY, items.len() as u8), 0);
                } else {
                    // A literal larger than the MKARRAY argc (u8): build ≤255-item
                    // chunks and concatenate them with MKARGS.
                    let mut nchunks = 0usize;
                    for chunk in items.chunks(255) {
                        for it in chunk {
                            self.compile_expr(b, it)?;
                        }
                        b.emit(Op::CallBuiltin(ops::MKARRAY, chunk.len() as u8), 0);
                        nchunks += 1;
                    }
                    b.emit(Op::CallBuiltin(ops::MKARGS, argc(nchunks)?), 0);
                }
            }
            Expr::Splat(e) => {
                // A bare splat outside a call/array just yields its array.
                self.compile_expr(b, e)?;
            }
            Expr::BlockPass(_) => {
                // A block-pass value (`&expr` / `...`) is only ever the body of a
                // synthetic pass-through block that `compile_call` unwraps; it is
                // never a standalone expression.
                return Err("block-pass used outside call position".into());
            }
            Expr::Lambda(block) => {
                // A lambda is a Proc value: compile its body as a proc template.
                // `MKLAMBDA` (vs `MKPROC`) flags it so `lambda?` returns `true`.
                let id = self.compile_proc(block)?;
                b.emit(Op::LoadInt(id as i64), 0);
                b.emit(Op::CallBuiltin(ops::MKLAMBDA, 1), 0);
            }
            Expr::Regex(source, flags) => {
                // `/…#{expr}…/` — build the pattern string at runtime by compiling
                // the interpolation like a double-quoted string, then compile the
                // Regexp. A static pattern (no `#{`) pushes the source directly.
                if source.contains("#{") {
                    let parts = crate::parser::scan_interp(source)?;
                    self.compile_expr(b, &Expr::Str(parts))?;
                } else {
                    self.kstr(b, source);
                }
                self.kstr(b, flags);
                b.emit(Op::CallBuiltin(ops::MKREGEX, 2), 0);
            }
            Expr::Hash(pairs) => {
                if pairs.len() * 2 <= 255 {
                    for (k, v) in pairs {
                        self.compile_expr(b, k)?;
                        self.compile_expr(b, v)?;
                    }
                    b.emit(Op::CallBuiltin(ops::MKHASH, (pairs.len() * 2) as u8), 0);
                } else {
                    // A literal larger than the MKHASH argc (u8, so >127 pairs):
                    // build ≤127-pair sub-hashes and merge them (later wins).
                    let mut nchunks = 0usize;
                    for chunk in pairs.chunks(127) {
                        for (k, v) in chunk {
                            self.compile_expr(b, k)?;
                            self.compile_expr(b, v)?;
                        }
                        b.emit(Op::CallBuiltin(ops::MKHASH, (chunk.len() * 2) as u8), 0);
                        nchunks += 1;
                    }
                    b.emit(Op::CallBuiltin(ops::MKHASH_MERGE, argc(nchunks)?), 0);
                }
            }
            Expr::Range { lo, hi, exclusive } => {
                // An absent bound compiles to a sentinel the runtime recognizes.
                match lo {
                    Some(e) => self.compile_expr(b, e)?,
                    None => {
                        b.emit(Op::LoadInt(crate::host::RANGE_BEGINLESS), 0);
                    }
                }
                match hi {
                    Some(e) => self.compile_expr(b, e)?,
                    None => {
                        b.emit(Op::LoadInt(crate::host::RANGE_ENDLESS), 0);
                    }
                }
                b.emit(
                    if *exclusive {
                        Op::LoadTrue
                    } else {
                        Op::LoadFalse
                    },
                    0,
                );
                b.emit(Op::CallBuiltin(ops::MKRANGE, 3), 0);
            }
            // A bare identifier equal to the enclosing method's name, not shadowed
            // by a local, is a self-recursive call — compile it as one so it
            // actually re-enters the method (`countdown`, Journey's
            // `parse_expressions`), instead of reading a nil local.
            Expr::Var(VarKind::Local, name)
                if self.cur_method.last().map(String::as_str) == Some(name.as_str())
                    && !self.scope_locals.last().is_some_and(|s| s.contains(name)) =>
            {
                self.compile_call(b, &None, name, &[], &None)?;
            }
            Expr::Var(kind, name) => self.compile_var_read(b, *kind, name),
            Expr::Assign(target, value) => self.compile_assign(b, target, value)?,
            Expr::MultiAssign { targets, values } => {
                self.compile_multi_assign(b, targets, values)?
            }
            Expr::Unary(op, e) => self.compile_unary(b, *op, e)?,
            Expr::Binary(op, l, r) => self.compile_binary(b, *op, l, r)?,
            Expr::If {
                cond,
                then,
                elifs,
                els,
            } => self.compile_if(b, cond, then, elifs, els)?,
            Expr::While { cond, body } => self.compile_while(b, cond, body)?,
            Expr::DoWhile { cond, body } => self.compile_do_while(b, cond, body)?,
            Expr::For { var, iter, body } => self.compile_for(b, var, iter, body)?,
            Expr::Case {
                subject,
                whens,
                els,
            } => self.compile_case(b, subject, whens, els)?,
            Expr::CaseIn {
                subject,
                clauses,
                els,
            } => self.compile_case_in(b, subject, clauses, els)?,
            Expr::Call {
                recv,
                name,
                args,
                block,
            } => self.compile_call(b, recv, name, args, block)?,
            Expr::Index(recv, idx) => {
                if idx.iter().any(|i| matches!(i, Expr::Splat(_))) {
                    // Spread index args (`Hash[*pairs]`, `a[*idx]`) → `recv.[](*idx)`.
                    self.compile_expr(b, recv)?;
                    self.kstr(b, "[]");
                    self.compile_spread(b, idx)?;
                    b.emit(Op::CallBuiltin(ops::CALL_METHOD_ARR, 3), self.cur_line);
                } else {
                    self.compile_expr(b, recv)?;
                    for i in idx {
                        self.compile_expr(b, i)?;
                    }
                    b.emit(
                        Op::CallBuiltin(ops::INDEX_GET, argc(1 + idx.len())?),
                        self.cur_line,
                    );
                }
            }
            Expr::Def {
                name,
                params,
                body,
                singleton,
                singleton_recv,
            } => {
                match singleton_recv {
                    // `def obj.m` / `def Klass.m` — evaluate the receiver and
                    // register the body as a per-object singleton method (or, for
                    // a class receiver, a class method) at runtime.
                    Some(recv) => {
                        let synth = self.stash_method(params, body)?;
                        self.compile_expr(b, recv)?;
                        self.kstr(b, name);
                        self.kstr(b, &synth);
                        b.emit(Op::CallBuiltin(ops::DEFINE_SINGLETON, 3), 0);
                    }
                    // `def self.m` in a runtime position (inside a conditional /
                    // loop, not the class body's top level where it is extracted
                    // at compile time) — register as a singleton method on the
                    // current `self`, i.e. a class method when self is a class
                    // (concurrent-ruby defines `def self.full_memory_barrier`
                    // inside a `case … else`).
                    None if *singleton => {
                        let synth = self.stash_method(params, body)?;
                        self.compile_expr(b, &Expr::SelfExpr)?;
                        self.kstr(b, name);
                        self.kstr(b, &synth);
                        b.emit(Op::CallBuiltin(ops::DEFINE_SINGLETON, 3), 0);
                    }
                    // A plain `def`: hoisted as a top-level method (available
                    // before the line runs), plus a runtime `DEFINE_METHOD_DYN`
                    // that registers it on the active `class_eval`/`instance_eval`
                    // target when one is in effect. `def` evaluates to `:name`.
                    None => {
                        let def = self.compile_method_named(name, params, body)?;
                        let synth = format!("__def{}__", next_synth_id());
                        self.methods.push((name.clone(), def.clone()));
                        self.methods.push((synth.clone(), def));
                        self.kstr(b, name);
                        self.kstr(b, &synth);
                        b.emit(Op::CallBuiltin(ops::DEFINE_METHOD_DYN, 2), 0);
                    }
                }
            }
            Expr::Class {
                name,
                superclass,
                body,
            } => self.compile_class(b, name, superclass, body)?,
            Expr::Module { name, body } => self.compile_class(b, name, &None, body)?,
            // `class << recv … end` outside a class body. Each inner `def`
            // becomes a singleton method of `recv` (or the current `self` for
            // `class << self`), registered at runtime. Evaluates to nil.
            Expr::SingletonClass { recv, body } => {
                let mut nondefs: Vec<Stmt> = Vec::new();
                for s in body {
                    if let Expr::Def {
                        name, params, body, ..
                    } = &s.expr
                    {
                        let synth = self.stash_method(params, body)?;
                        match recv {
                            Some(r) => self.compile_expr(b, r)?,
                            None => {
                                b.emit(Op::CallBuiltin(ops::GETSELF, 0), 0);
                            }
                        }
                        self.kstr(b, name);
                        self.kstr(b, &synth);
                        b.emit(Op::CallBuiltin(ops::DEFINE_SINGLETON, 3), 0);
                        b.emit(Op::Pop, 0);
                    } else {
                        nondefs.push(s.clone());
                    }
                }
                // Non-`def` statements (`alias_method`, `attr_*`, directives) run
                // with `self` = the singleton class, i.e. `<recv>.singleton_class
                // .class_eval { … }`, so they operate on the class's own methods.
                if !nondefs.is_empty() {
                    let recv_expr = match recv {
                        Some(r) => (**r).clone(),
                        None => Expr::SelfExpr,
                    };
                    let call = Expr::Call {
                        recv: Some(Box::new(Expr::Call {
                            recv: Some(Box::new(recv_expr)),
                            name: "singleton_class".into(),
                            args: Vec::new(),
                            block: None,
                        })),
                        name: "class_eval".into(),
                        args: Vec::new(),
                        block: Some(Block {
                            params: Vec::new(),
                            splat: None,
                            body: nondefs,
                            arity: None,
                        }),
                    };
                    self.compile_expr(b, &call)?;
                    b.emit(Op::Pop, 0);
                }
                b.emit(Op::LoadUndef, 0);
            }
            Expr::SelfExpr => {
                b.emit(Op::CallBuiltin(ops::GETSELF, 0), 0);
            }
            Expr::Begin {
                body,
                rescues,
                ensure,
            } => self.compile_begin(b, body, rescues, ensure)?,
            Expr::Return(e) => self.compile_flow(b, e, ops::SIG_RETURN, FlowKind::Return)?,
            Expr::Break(e) => self.compile_flow(b, e, ops::SIG_BREAK, FlowKind::Break)?,
            Expr::Next(e) => self.compile_flow(b, e, ops::SIG_NEXT, FlowKind::Next)?,
            Expr::Retry => {
                // `retry` is always a signal (never a native loop jump): push a
                // dummy value the builtin pops, then raise the retry signal.
                b.emit(Op::LoadUndef, 0);
                b.emit(Op::CallBuiltin(ops::SIG_RETRY, 1), 0);
            }
            Expr::Redo => {
                // Inside a native `while`/`until`, `redo` is a backward jump to
                // the body start (skipping the condition test). Anywhere else —
                // notably a block body, which compiles to its own chunk with an
                // empty loop stack — it is a signal `call_proc` re-runs on.
                if self.loops.is_empty() {
                    // Outside both a loop and a block there is nothing to re-run.
                    // MRI rejects this at parse time; reject it here rather than
                    // leaving a signal nothing consumes (which would silently
                    // truncate the program).
                    if !self.redo_ok {
                        return Err("Invalid redo (SyntaxError)".into());
                    }
                    b.emit(Op::LoadUndef, 0); // dummy operand the builtin pops
                    b.emit(Op::CallBuiltin(ops::SIG_REDO, 1), 0);
                } else {
                    let j = b.emit(Op::Jump(0), 0);
                    self.loops.last_mut().unwrap().redos.push(j);
                    b.emit(Op::LoadUndef, 0); // leave a value (dead if jump taken)
                }
            }
            Expr::Yield(args) => {
                for a in args {
                    self.compile_expr(b, a)?;
                }
                b.emit(
                    Op::CallBuiltin(ops::YIELD, argc(args.len())?),
                    self.cur_line,
                );
            }
            Expr::Defined(operand) => self.compile_defined(b, operand)?,
            Expr::Super { args, block } => {
                // A pass-through block (`super(&blk)` / `super(...)`) emits its
                // value directly as the block operand, like `compile_call`; an
                // ordinary block literal builds a Proc via `MkProc`.
                let pass_expr = block.as_ref().and_then(block_pass_expr);
                let proc_id = match (&pass_expr, block) {
                    (Some(_), _) => None,
                    (None, Some(bl)) => Some(self.compile_proc(bl)?),
                    (None, None) => None,
                };
                let has_block = pass_expr.is_some() || proc_id.is_some();
                let emit_block = |s: &mut Self, b: &mut ChunkBuilder| -> Result<(), String> {
                    match (&pass_expr, proc_id) {
                        (Some(e), _) => s.compile_expr(b, e),
                        (None, Some(id)) => {
                            b.emit(Op::LoadInt(id as i64), 0);
                            b.emit(Op::CallBuiltin(ops::MKPROC, 1), 0);
                            Ok(())
                        }
                        (None, None) => Ok(()),
                    }
                };
                match (args, has_block) {
                    // `super(args)` — explicit args, forward the current block.
                    (Some(args), false) => {
                        for a in args {
                            self.compile_expr(b, a)?;
                        }
                        b.emit(
                            Op::CallBuiltin(ops::SUPER, argc(args.len())?),
                            self.cur_line,
                        );
                    }
                    // `super` — forward args and block.
                    (None, false) => {
                        b.emit(Op::CallBuiltin(ops::SUPER_FWD, 0), self.cur_line);
                    }
                    // `super(args) { blk }` / `super(args, &blk)` — explicit args +
                    // a block (the block value rides on top of the args).
                    (Some(args), true) => {
                        for a in args {
                            self.compile_expr(b, a)?;
                        }
                        emit_block(self, b)?;
                        b.emit(
                            Op::CallBuiltin(ops::SUPER_BLK, argc(args.len() + 1)?),
                            self.cur_line,
                        );
                    }
                    // `super { blk }` / `super(&blk)` — forward args, pass a block.
                    (None, true) => {
                        emit_block(self, b)?;
                        b.emit(Op::CallBuiltin(ops::SUPER_FWD_BLK, 1), self.cur_line);
                    }
                }
            }
        }
        Ok(())
    }

    /// Qualify a name with the innermost enclosing namespace: `X` inside
    /// `module A::B` becomes `A::B::X`. At the top level (`nesting` empty) the
    /// name is returned unchanged. This is the storage key for a constant or a
    /// nested class/module.
    fn qualify(&self, name: &str) -> String {
        match self.nesting.last() {
            Some(ns) => format!("{ns}::{name}"),
            None => name.to_string(),
        }
    }

    /// Resolve a written class/module reference (`Base`, `Foo::Base`) used as a
    /// superclass or mixin argument to an already-registered qualified name.
    /// Tries, innermost namespace first, `<ns>::<written>`, then `<written>`
    /// itself, returning the first that a class/module was registered under. If
    /// none match (a forward or builtin reference) the written name is kept.
    fn resolve_class_name(&self, written: &str) -> String {
        // A leading `::` is an absolute (top-level) reference — never qualify it
        // with the lexical nesting (`class C < ::Rails::Railtie::Configuration`
        // must not become `Rails::Engine::::Rails::…`, which would leave the `::`
        // prefix and break superclass resolution).
        if let Some(abs) = written.strip_prefix("::") {
            return abs.to_string();
        }
        for ns in self.nesting.iter().rev() {
            let cand = format!("{ns}::{written}");
            if self.classes.iter().any(|(n, _)| *n == cand) {
                return cand;
            }
        }
        written.to_string()
    }

    /// The lexical-lookup candidate chain for a bare constant read, encoded as a
    /// `\x1f`-separated string the `GETCONST` builtin splits and tries in order.
    /// For `X` inside `module A; module B` this is `A::B::X`, `A::X`, `X`
    /// (innermost nesting first, then the top level) — Ruby's constant search.
    fn const_candidates(&self, name: &str) -> String {
        // A `::`-prefixed name is absolute (top-level `::Logger`): strip the marker
        // and resolve only the bare name, ignoring the lexical nesting.
        if let Some(abs) = name.strip_prefix("::") {
            return abs.to_string();
        }
        if self.nesting.is_empty() {
            return name.to_string();
        }
        let mut cands: Vec<String> = self
            .nesting
            .iter()
            .rev()
            .map(|ns| format!("{ns}::{name}"))
            .collect();
        cands.push(name.to_string());
        cands.join("\u{1f}")
    }

    /// Plan slot lowering for a scope about to be compiled and push the map onto
    /// `slot_scopes`. Slot candidates are the scope's plainly-assigned locals,
    /// minus `params` (host-bound until the calling convention lands) and minus
    /// captured/introspected names (`slot_scan`). No pre-`nil` store is needed: an
    /// unset fusevm slot reads back as `Undef`, which is exactly how rubylang
    /// represents `nil` (`Expr::Nil` lowers to `Op::LoadUndef`), so a read before
    /// the first assignment already yields Ruby's hoisted `nil`.
    fn enter_slot_scope(&mut self, params: &std::collections::HashSet<String>, body: &[Stmt]) {
        let (caps, disq) = slot_scan(body);
        let mut slots = std::collections::HashMap::new();
        if !disq {
            let mut idx = 0u16;
            for name in scope_plain_assigns(body) {
                if params.contains(&name) || caps.contains(&name) {
                    continue;
                }
                slots.insert(name, idx);
                idx += 1;
            }
        }
        self.slot_scopes.push(slots);
    }

    fn exit_slot_scope(&mut self) {
        self.slot_scopes.pop();
    }

    /// Plan slot lowering for a method body, additionally binding simple
    /// positional params into the leading frame slots `0..N` when it is safe (the
    /// dispatcher seeds those slots from the call's positional args before running
    /// the chunk). Returns `N` (`slot_params`); 0 keeps every param host-bound.
    ///
    /// Params slot-bind only for a *simple* signature — all required positional,
    /// no default/splat/keyword/kwsplat/block param, no parameter-default local,
    /// none captured by a nested closure — and a body the analysis didn't
    /// disqualify. Otherwise params (and default locals) stay host-bound and only
    /// body locals slot-lower, exactly as `enter_slot_scope`.
    fn enter_method_slot_scope(
        &mut self,
        params: &[Param],
        default_locals: &[String],
        body: &[Stmt],
    ) -> u16 {
        let (caps, disq) = slot_scan(body);
        let simple = !disq
            && !params.is_empty()
            && default_locals.is_empty()
            && params
                .iter()
                .all(|p| !p.keyword && !p.kwsplat && !p.block && !p.splat && p.default.is_none())
            && params.iter().all(|p| !caps.contains(&p.name));
        if !simple {
            // Fall back to body-only slot lowering; params stay host-bound.
            let mut host_bound: std::collections::HashSet<String> =
                params.iter().map(|p| p.name.clone()).collect();
            host_bound.extend(default_locals.iter().cloned());
            self.enter_slot_scope(&host_bound, body);
            return 0;
        }
        // Params occupy slots `0..N` in declaration order; body locals follow.
        let mut slots = std::collections::HashMap::new();
        let mut idx = 0u16;
        for p in params {
            slots.insert(p.name.clone(), idx);
            idx += 1;
        }
        let n_params = idx;
        for name in scope_plain_assigns(body) {
            if slots.contains_key(&name) || caps.contains(&name) {
                continue;
            }
            slots.insert(name, idx);
            idx += 1;
        }
        self.slot_scopes.push(slots);
        n_params
    }

    /// The frame slot assigned to local `name` in the current scope, if it was
    /// slot-lowered (else `None`, meaning the host `GETLOCAL`/`SETLOCAL` path).
    fn local_slot(&self, name: &str) -> Option<u16> {
        self.slot_scopes.last().and_then(|m| m.get(name)).copied()
    }

    fn compile_var_read(&mut self, b: &mut ChunkBuilder, kind: VarKind, name: &str) {
        // A constant read inside a namespace lowers to a candidate chain so the
        // runtime can walk the lexical nesting; every other var reads by name.
        if let VarKind::Const = kind {
            let encoded = self.const_candidates(name);
            self.kstr(b, &encoded);
            b.emit(Op::CallBuiltin(ops::GETCONST, 1), 0);
            return;
        }
        // A slot-lowered local reads from the frame slot (native-lowerable) rather
        // than a host dispatch.
        if let VarKind::Local = kind {
            if let Some(slot) = self.local_slot(name) {
                b.emit(Op::GetSlot(slot), self.cur_line);
                return;
            }
        }
        let op = match kind {
            VarKind::Local => ops::GETLOCAL,
            VarKind::Instance => ops::GETIVAR,
            VarKind::Class => ops::GETCVAR,
            VarKind::Global => ops::GETGVAR,
            VarKind::Const => ops::GETCONST,
        };
        self.kstr(b, name);
        // A bare-name read (`GETLOCAL`, …) can turn into a zero-arg method call
        // that raises, so carry the line for the backtrace.
        b.emit(Op::CallBuiltin(op, 1), self.cur_line);
    }

    fn compile_assign(
        &mut self,
        b: &mut ChunkBuilder,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), String> {
        match target {
            Expr::Var(kind, name) => {
                if let VarKind::Local = kind {
                    // From this assignment on, the name is a local in this scope
                    // (so a later bare read of it is a variable, not a call).
                    if let Some(scope) = self.scope_locals.last_mut() {
                        scope.insert(name.clone());
                    }
                    // A slot-lowered local writes its frame slot. Assignment is an
                    // expression yielding the RHS, and `SetSlot` pops without
                    // pushing, so `Dup` the value to leave it on the stack (matching
                    // `SETLOCAL`, whose builtin returns the assigned value).
                    if let Some(slot) = self.local_slot(name) {
                        self.compile_expr(b, value)?;
                        b.emit(Op::Dup, 0);
                        b.emit(Op::SetSlot(slot), 0);
                        return Ok(());
                    }
                }
                let op = match kind {
                    VarKind::Local => ops::SETLOCAL,
                    VarKind::Instance => ops::SETIVAR,
                    VarKind::Class => ops::SETCVAR,
                    VarKind::Global => ops::SETGVAR,
                    VarKind::Const => ops::SETCONST,
                };
                // A constant assignment inside a namespace defines it under the
                // qualified name (`X = 1` in `module A::B` sets `A::B::X`).
                let stored = if let VarKind::Const = kind {
                    self.qualify(name)
                } else {
                    name.to_string()
                };
                self.kstr(b, &stored);
                self.compile_expr(b, value)?;
                b.emit(Op::CallBuiltin(op, 2), 0);
            }
            Expr::Index(recv, idx) => {
                // `recv[i] = v` evaluates to `v` — never `[]=`'s return value
                // (Ruby drops it). Evaluate operands into temps in source order,
                // call `[]=`, discard its result, yield the RHS.
                let tr = self.eval_to_temp(b, recv)?;
                let tis = idx
                    .iter()
                    .map(|i| self.eval_to_temp(b, i))
                    .collect::<Result<Vec<_>, _>>()?;
                let tv = self.eval_to_temp(b, value)?;
                self.compile_expr(b, &Expr::Var(VarKind::Local, tr))?;
                for ti in &tis {
                    self.compile_expr(b, &Expr::Var(VarKind::Local, ti.clone()))?;
                }
                self.compile_expr(b, &Expr::Var(VarKind::Local, tv.clone()))?;
                b.emit(
                    Op::CallBuiltin(ops::INDEX_SET, argc(2 + idx.len())?),
                    self.cur_line,
                );
                b.emit(Op::Pop, 0);
                self.compile_expr(b, &Expr::Var(VarKind::Local, tv))?;
            }
            // `A::B = v` (a capitalized name on a constant-path receiver) is a
            // namespaced constant assignment, stored under the qualified name —
            // not a `B=` setter call.
            Expr::Call {
                recv: Some(r),
                name,
                args,
                block: None,
            } if args.is_empty()
                && name.chars().next().is_some_and(|c| c.is_uppercase())
                && const_path_name(r).is_some() =>
            {
                let qualified = format!("{}::{name}", const_path_name(r).unwrap());
                self.kstr(b, &qualified);
                self.compile_expr(b, value)?;
                b.emit(Op::CallBuiltin(ops::SETCONST, 2), 0);
            }
            // Attribute assignment: `recv.attr = v` is the setter call
            // `recv.attr=(v)`.
            Expr::Call {
                recv: Some(r),
                name,
                args,
                block: None,
            } if args.is_empty() => {
                // `recv.attr = v` evaluates to `v`, never the setter's return
                // value (Ruby discards it). Evaluate `recv` then `v` into temps
                // (source order), call the setter, drop its result, yield `v`.
                let tr = self.eval_to_temp(b, r)?;
                let tv = self.eval_to_temp(b, value)?;
                self.compile_expr(b, &Expr::Var(VarKind::Local, tr))?;
                self.kstr(b, &format!("{name}="));
                self.compile_expr(b, &Expr::Var(VarKind::Local, tv.clone()))?;
                b.emit(Op::CallBuiltin(ops::CALL_METHOD, 3), self.cur_line);
                b.emit(Op::Pop, 0);
                self.compile_expr(b, &Expr::Var(VarKind::Local, tv))?;
            }
            _ => return Err("invalid assignment target".into()),
        }
        Ok(())
    }

    /// Evaluate `e` once and stash it in a fresh synthetic local, leaving the
    /// stack clean; returns the local's name. Used to reorder operand evaluation
    /// out of a builder while yielding a value the caller loads back later.
    fn eval_to_temp(&mut self, b: &mut ChunkBuilder, e: &Expr) -> Result<String, String> {
        self.tmp += 1;
        let name = format!("__asgn{}__", self.tmp);
        self.compile_assign(b, &Expr::Var(VarKind::Local, name.clone()), e)?;
        b.emit(Op::Pop, 0);
        Ok(name)
    }

    /// Parallel assignment: normalize the right-hand side to an array in a
    /// synthetic local, assign each target from its index, and yield the array.
    fn compile_multi_assign(
        &mut self,
        b: &mut ChunkBuilder,
        targets: &[Expr],
        values: &[Expr],
    ) -> Result<(), String> {
        let tmp = "__massign__";
        // rhs = a single Array or the coerced value list. A lone `*x` value
        // (`a, b = *x`) is just `x` coerced to an array — unwrap the splat so it
        // is not spread into `Array`'s own arguments.
        let rhs = if values.len() == 1 {
            let inner = match &values[0] {
                Expr::Splat(e) => e.as_ref().clone(),
                other => other.clone(),
            };
            Expr::Call {
                recv: None,
                name: "Array".into(),
                args: vec![inner],
                block: None,
            }
        } else {
            Expr::Array(values.to_vec())
        };
        self.compile_assign(b, &Expr::Var(VarKind::Local, tmp.into()), &rhs)?;
        b.emit(Op::Pop, 0);
        let tmp_var = || Expr::Var(VarKind::Local, tmp.into());
        let splat_at = targets.iter().position(|t| matches!(t, Expr::Splat(_)));
        for (i, t) in targets.iter().enumerate() {
            // Value expression the target is assigned from.
            let (target, value): (&Expr, Expr) = match (splat_at, t) {
                // The splat target collects `rhs[i .. len - after]` as an array.
                (Some(si), Expr::Splat(inner)) if i == si => {
                    let after = targets.len() - si - 1;
                    // length = rhs.length - after - si
                    let len_expr = Expr::Binary(
                        BinOp::Sub,
                        Box::new(Expr::Binary(
                            BinOp::Sub,
                            Box::new(Expr::Call {
                                recv: Some(Box::new(tmp_var())),
                                name: "length".into(),
                                args: vec![],
                                block: None,
                            }),
                            Box::new(Expr::Int(after as i64)),
                        )),
                        Box::new(Expr::Int(si as i64)),
                    );
                    let slice =
                        Expr::Index(Box::new(tmp_var()), vec![Expr::Int(si as i64), len_expr]);
                    (inner.as_ref(), slice)
                }
                // A target after the splat indexes from the end.
                (Some(si), t) if i > si => {
                    let from_end = targets.len() - i; // 1-based from the end
                    let idx = Expr::Binary(
                        BinOp::Sub,
                        Box::new(Expr::Call {
                            recv: Some(Box::new(tmp_var())),
                            name: "length".into(),
                            args: vec![],
                            block: None,
                        }),
                        Box::new(Expr::Int(from_end as i64)),
                    );
                    (t, Expr::Index(Box::new(tmp_var()), vec![idx]))
                }
                // A plain positional target.
                (_, t) => (
                    t,
                    Expr::Index(Box::new(tmp_var()), vec![Expr::Int(i as i64)]),
                ),
            };
            self.compile_assign(b, target, &value)?;
            b.emit(Op::Pop, 0);
        }
        self.compile_var_read(b, VarKind::Local, tmp);
        Ok(())
    }

    fn compile_unary(&mut self, b: &mut ChunkBuilder, op: UnOp, e: &Expr) -> Result<(), String> {
        self.compile_expr(b, e)?;
        match op {
            UnOp::Neg => {
                b.emit(Op::Negate, 0);
            }
            UnOp::Not => {
                b.emit(Op::RubyTruthy, 0);
                b.emit(Op::LogNot, 0);
            }
            UnOp::BitNot => {
                b.emit(Op::BitNot, 0);
            }
        }
        Ok(())
    }

    fn compile_binary(
        &mut self,
        b: &mut ChunkBuilder,
        op: BinOp,
        l: &Expr,
        r: &Expr,
    ) -> Result<(), String> {
        // Short-circuit operators keep the operand values (Ruby semantics).
        if op == BinOp::And || op == BinOp::Or {
            self.compile_expr(b, l)?;
            b.emit(Op::Dup, 0);
            b.emit(Op::RubyTruthy, 0);
            let jmp = if op == BinOp::And {
                b.emit(Op::JumpIfFalse(0), 0)
            } else {
                b.emit(Op::JumpIfTrue(0), 0)
            };
            b.emit(Op::Pop, 0);
            self.compile_expr(b, r)?;
            let end = b.current_pos();
            b.patch_jump(jmp, end);
            return Ok(());
        }
        // `**`, `/` and `%` go through host dispatch so integer operands keep
        // Ruby semantics: `Integer ** Integer` and `Integer / Integer` stay
        // Integer (fusevm's native ops produce a Float), and division/modulo
        // floor toward negative infinity (fusevm truncates).
        // `<<` and `>>` are `Array#<<`/`String#<<`/`Integer#<<` in Ruby, not the
        // VM's bit-shift; `**`/`/`/`%` need Ruby integer semantics. Route them
        // all through host method dispatch.
        // `<=>` also routes through dispatch so a user `def <=>` and Comparable
        // work; the native `Spaceship` op would not consult a user method.
        if matches!(
            op,
            BinOp::Pow
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::Cmp
                | BinOp::CaseEq
                | BinOp::Match
                | BinOp::NMatch
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
        ) {
            let name = match op {
                BinOp::Pow => "**",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
                BinOp::Cmp => "<=>",
                BinOp::CaseEq => "===",
                // `&`/`|`/`^` are methods (Integer bit ops, Set/Array algebra,
                // and user operator overloads), so dispatch rather than the
                // native VM op.
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                _ => "=~", // Match and NMatch both dispatch =~
            };
            self.compile_expr(b, l)?;
            self.kstr(b, name);
            self.compile_expr(b, r)?;
            b.emit(Op::CallBuiltin(ops::CALL_METHOD, 3), 0);
            // `!~` is the negation of `=~`.
            if op == BinOp::NMatch {
                b.emit(Op::RubyTruthy, 0);
                b.emit(Op::LogNot, 0);
            }
            return Ok(());
        }
        self.compile_expr(b, l)?;
        self.compile_expr(b, r)?;
        let native = match op {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Div => Op::Div,
            BinOp::Mod => Op::Mod,
            BinOp::Pow => Op::Pow,
            BinOp::Eq => Op::NumEq,
            BinOp::Ne => Op::NumNe,
            BinOp::Lt => Op::NumLt,
            BinOp::Gt => Op::NumGt,
            BinOp::Le => Op::NumLe,
            BinOp::Ge => Op::NumGe,
            BinOp::Cmp => Op::Spaceship,
            BinOp::BitAnd => Op::BitAnd,
            BinOp::BitOr => Op::BitOr,
            BinOp::BitXor => Op::BitXor,
            BinOp::Shl => Op::Shl,
            BinOp::Shr => Op::Shr,
            BinOp::And | BinOp::Or | BinOp::Match | BinOp::NMatch | BinOp::CaseEq => unreachable!(),
        };
        b.emit(native, 0);
        Ok(())
    }

    fn compile_cond(&mut self, b: &mut ChunkBuilder, cond: &Expr) -> Result<(), String> {
        self.compile_expr(b, cond)?;
        b.emit(Op::RubyTruthy, 0);
        Ok(())
    }

    fn compile_if(
        &mut self,
        b: &mut ChunkBuilder,
        cond: &Expr,
        then: &[Stmt],
        elifs: &[(Expr, Vec<Stmt>)],
        els: &Option<Vec<Stmt>>,
    ) -> Result<(), String> {
        let mut end_jumps = Vec::new();
        // primary
        self.compile_cond(b, cond)?;
        let mut skip = b.emit(Op::JumpIfFalse(0), 0);
        self.compile_seq(b, then)?;
        end_jumps.push(b.emit(Op::Jump(0), 0));
        // elsifs
        for (c, body) in elifs {
            let here = b.current_pos();
            b.patch_jump(skip, here);
            self.compile_cond(b, c)?;
            skip = b.emit(Op::JumpIfFalse(0), 0);
            self.compile_seq(b, body)?;
            end_jumps.push(b.emit(Op::Jump(0), 0));
        }
        // else
        let else_pos = b.current_pos();
        b.patch_jump(skip, else_pos);
        match els {
            Some(body) => self.compile_seq(b, body)?,
            None => {
                b.emit(Op::LoadUndef, 0);
            }
        }
        let end = b.current_pos();
        for j in end_jumps {
            b.patch_jump(j, end);
        }
        Ok(())
    }

    fn compile_while(
        &mut self,
        b: &mut ChunkBuilder,
        cond: &Expr,
        body: &[Stmt],
    ) -> Result<(), String> {
        let start = b.current_pos();
        self.loops.push(LoopCtx {
            start,
            body_start: start,
            breaks: vec![],
            nexts: vec![],
            redos: vec![],
        });
        self.compile_cond(b, cond)?;
        let exit = b.emit(Op::JumpIfFalse(0), 0);
        // `redo` re-runs the body without re-testing the condition.
        self.loops.last_mut().unwrap().body_start = b.current_pos();
        self.compile_seq(b, body)?;
        b.emit(Op::Pop, 0); // discard the body value each iteration
        b.emit(Op::Jump(start), 0);
        b.patch_jump(exit, b.current_pos());
        // Running out of iterations makes the loop nil; a `break` lands *past*
        // this, carrying its own operand as the loop's value.
        b.emit(Op::LoadUndef, 0);
        let end = b.current_pos();
        let ctx = self.loops.pop().unwrap();
        for j in ctx.breaks {
            b.patch_jump(j, end);
        }
        for j in ctx.nexts {
            b.patch_jump(j, ctx.start);
        }
        for j in ctx.redos {
            b.patch_jump(j, ctx.body_start);
        }
        Ok(())
    }

    /// `begin … end while cond` / `… until cond`: a post-test loop. The body is
    /// compiled first (so it always runs at least once), then the condition is
    /// checked and, if truthy, control jumps back to the body. `next` targets the
    /// condition check; `break` exits. Like `while`, it evaluates to nil.
    fn compile_do_while(
        &mut self,
        b: &mut ChunkBuilder,
        cond: &Expr,
        body: &[Stmt],
    ) -> Result<(), String> {
        let body_start = b.current_pos();
        self.loops.push(LoopCtx {
            start: body_start,
            body_start,
            breaks: vec![],
            nexts: vec![],
            redos: vec![],
        });
        self.compile_seq(b, body)?;
        b.emit(Op::Pop, 0); // discard the body value each iteration
        let cond_pos = b.current_pos();
        self.compile_cond(b, cond)?;
        b.emit(Op::JumpIfTrue(body_start), 0);
        // Same shape as `while`: nil on a normal exit, the operand on a `break`.
        b.emit(Op::LoadUndef, 0);
        let end = b.current_pos();
        let ctx = self.loops.pop().unwrap();
        for j in ctx.breaks {
            b.patch_jump(j, end);
        }
        for j in ctx.nexts {
            b.patch_jump(j, cond_pos);
        }
        for j in ctx.redos {
            b.patch_jump(j, ctx.body_start);
        }
        Ok(())
    }

    fn compile_for(
        &mut self,
        b: &mut ChunkBuilder,
        var: &str,
        iter: &Expr,
        body: &[Stmt],
    ) -> Result<(), String> {
        // `for v in iter … end` runs the body in the ENCLOSING scope — unlike
        // `iter.each { |v| … }`, Ruby's `for` introduces no scope at all, so `v`
        // outlives the loop and every closure made in the body shares the one
        // binding (`for i in 0..2 { procs << -> { i } }` yields `[2, 2, 2]`,
        // where the `each` form yields `[0, 1, 2]`).
        //
        // Desugaring to `each` with `v` as the *block parameter* would get both
        // of those wrong, since a block parameter is block-local. Bind a hidden
        // parameter instead and copy it into the enclosing local.
        self.tmp += 1;
        let hidden = format!("__for{}__", self.tmp);
        // Declare the loop variable — and every local the body assigns — in the
        // enclosing scope, so each exists even if the body never runs, without
        // clobbering a value it already has (`x = 5; for x in []; end; p x` is
        // still 5). Pre-declaring the body locals is what makes them outlive the
        // loop: the lowered `each` block then writes through to these bindings
        // instead of creating block-local ones.
        let mut declare = vec![var.to_string()];
        for n in for_body_locals(body) {
            if n != var && !n.starts_with("__") {
                declare.push(n);
            }
        }
        for name in &declare {
            self.kstr(b, name);
            b.emit(Op::CallBuiltin(ops::DEFINED, 1), 0);
            let skip = b.emit(Op::JumpIfTrue(0), 0);
            self.compile_assign(b, &Expr::Var(VarKind::Local, name.clone()), &Expr::Nil)?;
            b.emit(Op::Pop, 0);
            b.patch_jump(skip, b.current_pos());
        }

        let mut inner = vec![Stmt::from(Expr::Assign(
            Box::new(Expr::Var(VarKind::Local, var.to_string())),
            Box::new(Expr::Var(VarKind::Local, hidden.clone())),
        ))];
        inner.extend(body.iter().cloned());
        let block = Block {
            params: vec![hidden],
            splat: None,
            body: inner,
            arity: None,
        };
        let call = Expr::Call {
            recv: Some(Box::new(iter.clone())),
            name: "each".into(),
            args: vec![],
            block: Some(block),
        };
        self.compile_expr(b, &call)
    }

    /// `case/in` pattern matching: bind the subject to a temporary, then try each
    /// clause's pattern test + variable bindings + optional guard, running the
    /// first that matches. No `else` raises `NoMatchingPatternError`.
    fn compile_case_in(
        &mut self,
        b: &mut ChunkBuilder,
        subject: &Expr,
        clauses: &[InClause],
        els: &Option<Vec<Stmt>>,
    ) -> Result<(), String> {
        self.tmp += 1;
        let tmp = format!("__casein{}__", self.tmp);
        let subj = Expr::Var(VarKind::Local, tmp.clone());
        // Evaluate the subject once into the temporary.
        self.compile_assign(b, &subj, subject)?;
        b.emit(Op::Pop, 0);

        let mut end_jumps = Vec::new();
        for clause in clauses {
            let (test, binds) = lower_pattern(&clause.pattern, &subj);
            // Pattern test: on failure, skip to the next clause (`JumpIfFalse`
            // consumes the condition).
            self.compile_cond(b, &test)?;
            let next = b.emit(Op::JumpIfFalse(0), 0);
            // Bindings (assignments) run once the shape matches.
            for bind in &binds {
                self.compile_expr(b, bind)?;
                b.emit(Op::Pop, 0);
            }
            // A guard runs after binding; failing it also falls through.
            let guard_next = if let Some(g) = &clause.guard {
                self.compile_cond(b, g)?;
                Some(b.emit(Op::JumpIfFalse(0), 0))
            } else {
                None
            };
            self.compile_seq(b, &clause.body)?;
            end_jumps.push(b.emit(Op::Jump(0), 0));
            // Land here on a failed test / guard.
            let here = b.current_pos();
            b.patch_jump(next, here);
            if let Some(j) = guard_next {
                b.patch_jump(j, here);
            }
        }
        // No clause matched: run `else`, or raise like Ruby.
        match els {
            Some(body) => self.compile_seq(b, body)?,
            None => {
                self.compile_expr(b, &subj)?;
                b.emit(Op::CallBuiltin(ops::NO_MATCH, 1), 0);
            }
        }
        let end = b.current_pos();
        for j in end_jumps {
            b.patch_jump(j, end);
        }
        Ok(())
    }

    fn compile_case(
        &mut self,
        b: &mut ChunkBuilder,
        subject: &Expr,
        whens: &[(Vec<Expr>, Vec<Stmt>)],
        els: &Option<Vec<Stmt>>,
    ) -> Result<(), String> {
        // Bind the subject to a synthetic local so it is evaluated once.
        let tmp = "__case__";
        self.kstr(b, tmp);
        self.compile_expr(b, subject)?;
        b.emit(Op::CallBuiltin(ops::SETLOCAL, 2), 0);
        b.emit(Op::Pop, 0);

        let mut end_jumps = Vec::new();
        for (labels, body) in whens {
            // Jump into this arm's body when any label matches.
            let mut into_body = Vec::new();
            for label in labels {
                if let Expr::Splat(inner) = label {
                    // `when *arr` — the array is expanded into candidates, each
                    // tested with `===`. Compile as `arr.any? { |__e| __e === subj }`
                    // so Range/Class/Regexp case-equality still applies per element.
                    let any = Expr::Call {
                        recv: Some(inner.clone()),
                        name: "any?".into(),
                        args: Vec::new(),
                        block: Some(Block {
                            params: vec!["__e".into()],
                            splat: None,
                            body: vec![Expr::Call {
                                recv: Some(Box::new(Expr::Var(VarKind::Local, "__e".into()))),
                                name: "===".into(),
                                args: vec![Expr::Var(VarKind::Local, tmp.into())],
                                block: None,
                            }
                            .into()],
                            arity: None,
                        }),
                    };
                    self.compile_expr(b, &any)?;
                    b.emit(Op::RubyTruthy, 0);
                    into_body.push(b.emit(Op::JumpIfTrue(0), 0));
                    continue;
                }
                // label === subject   (CALL_METHOD wants [recv, name, arg])
                self.compile_expr(b, label)?;
                self.kstr(b, "===");
                self.compile_var_read(b, VarKind::Local, tmp);
                b.emit(Op::CallBuiltin(ops::CALL_METHOD, 3), self.cur_line);
                b.emit(Op::RubyTruthy, 0);
                into_body.push(b.emit(Op::JumpIfTrue(0), 0));
            }
            // No label matched → skip this arm's body.
            let skip = b.emit(Op::Jump(0), 0);
            let body_pos = b.current_pos();
            for j in into_body {
                b.patch_jump(j, body_pos);
            }
            self.compile_seq(b, body)?;
            end_jumps.push(b.emit(Op::Jump(0), 0));
            let after = b.current_pos();
            b.patch_jump(skip, after);
        }
        match els {
            Some(body) => self.compile_seq(b, body)?,
            None => {
                b.emit(Op::LoadUndef, 0);
            }
        }
        let end = b.current_pos();
        for j in end_jumps {
            b.patch_jump(j, end);
        }
        Ok(())
    }

    fn compile_call(
        &mut self,
        b: &mut ChunkBuilder,
        recv: &Option<Box<Expr>>,
        name: &str,
        args: &[Expr],
        block: &Option<Block>,
    ) -> Result<(), String> {
        // A pass-through block (`&expr` / `...` forwarding) carries its value
        // expression rather than a proc body: emit that value as the block operand
        // directly. `MkProc` builds a real Proc for an ordinary block literal.
        let pass_expr = block.as_ref().and_then(block_pass_expr);
        let proc_id = match (&pass_expr, block) {
            (Some(_), _) => None,
            (None, Some(bl)) => Some(self.compile_proc(bl)?),
            (None, None) => None,
        };
        // Emit the block operand for a `*_BLK` op: the forwarded value, or a Proc
        // built from the block literal's id.
        let emit_block = |s: &mut Self, b: &mut ChunkBuilder| -> Result<(), String> {
            match (&pass_expr, proc_id) {
                (Some(e), _) => s.compile_expr(b, e),
                (None, Some(id)) => {
                    b.emit(Op::LoadInt(id as i64), 0);
                    b.emit(Op::CallBuiltin(ops::MKPROC, 1), 0);
                    Ok(())
                }
                (None, None) => Ok(()),
            }
        };
        let has_block = pass_expr.is_some() || proc_id.is_some();
        // A call with a splat argument builds its argument array at runtime and
        // uses the array-spreading call ops (with a block-carrying variant when a
        // block is also passed, e.g. `foo(*args, &blk)`).
        if args.iter().any(|a| matches!(a, Expr::Splat(_))) {
            if let Some(r) = recv {
                self.compile_expr(b, r)?;
            }
            self.kstr(b, name);
            self.compile_spread(b, args)?;
            match (recv.is_some(), has_block) {
                (true, true) => {
                    emit_block(self, b)?;
                    b.emit(Op::CallBuiltin(ops::CALL_METHOD_ARR_BLK, 4), self.cur_line);
                }
                (true, false) => {
                    b.emit(Op::CallBuiltin(ops::CALL_METHOD_ARR, 3), self.cur_line);
                }
                (false, true) => {
                    emit_block(self, b)?;
                    b.emit(Op::CallBuiltin(ops::CALL_ARR_BLK, 3), self.cur_line);
                }
                (false, false) => {
                    b.emit(Op::CallBuiltin(ops::CALL_ARR, 2), self.cur_line);
                }
            }
            return Ok(());
        }
        match recv {
            Some(r) => {
                self.compile_expr(b, r)?;
                self.kstr(b, name);
                for a in args {
                    self.compile_expr(b, a)?;
                }
                if has_block {
                    emit_block(self, b)?;
                    b.emit(
                        Op::CallBuiltin(ops::CALL_METHOD_BLK, argc(3 + args.len())?),
                        self.cur_line,
                    );
                } else {
                    b.emit(
                        Op::CallBuiltin(ops::CALL_METHOD, argc(2 + args.len())?),
                        self.cur_line,
                    );
                }
            }
            None => {
                self.kstr(b, name);
                for a in args {
                    self.compile_expr(b, a)?;
                }
                if has_block {
                    emit_block(self, b)?;
                    b.emit(
                        Op::CallBuiltin(ops::CALL_BLK, argc(2 + args.len())?),
                        self.cur_line,
                    );
                } else {
                    b.emit(
                        Op::CallBuiltin(ops::CALL, argc(1 + args.len())?),
                        self.cur_line,
                    );
                }
            }
        }
        Ok(())
    }

    fn compile_proc(&mut self, block: &Block) -> Result<usize, String> {
        // A block/lambda body is a legal home for `redo` (a `begin` body compiled
        // through `compile_proc_body` is not — it only inherits the depth of
        // whatever encloses it), so track the nesting here.
        let saved_redo = std::mem::replace(&mut self.redo_ok, true);
        let r = self.compile_proc_body_arity(
            &block.body,
            &block.params,
            block.splat,
            block.arity.clone(),
        );
        self.redo_ok = saved_redo;
        r
    }

    /// Emit code leaving a single array on the stack that is `items` flattened
    /// with splats expanded: each non-splat becomes a one-element array, each
    /// `*expr` contributes its array, and `MKARGS` concatenates them all.
    fn compile_spread(&mut self, b: &mut ChunkBuilder, items: &[Expr]) -> Result<(), String> {
        for it in items {
            match it {
                Expr::Splat(e) => self.compile_expr(b, e)?,
                other => {
                    self.compile_expr(b, other)?;
                    b.emit(Op::CallBuiltin(ops::MKARRAY, 1), 0);
                }
            }
        }
        b.emit(Op::CallBuiltin(ops::MKARGS, argc(items.len())?), 0);
        Ok(())
    }

    /// Compile a body into a proc template with the given params; return its id.
    fn compile_proc_body(
        &mut self,
        body: &[Stmt],
        params: &[String],
        splat: Option<usize>,
    ) -> Result<usize, String> {
        self.compile_proc_body_arity(body, params, splat, None)
    }

    /// As [`compile_proc_body`], with the written parameter shape a lambda is
    /// arity-checked against. `None` (a synthesized block) means every param is
    /// a plain required positional.
    fn compile_proc_body_arity(
        &mut self,
        body: &[Stmt],
        params: &[String],
        splat: Option<usize>,
        arity: Option<crate::ast::BlockArity>,
    ) -> Result<usize, String> {
        let arity = arity.unwrap_or_else(|| crate::ast::BlockArity {
            req: params.len().saturating_sub(splat.is_some() as usize) as u16,
            ..Default::default()
        });
        let chunk = self.compile_body_chunk(body)?;
        let id = self.procs.len();
        self.procs.push(ProcDef {
            params: params.to_vec(),
            splat,
            chunk,
            arity,
        });
        Ok(id)
    }

    /// Lower a `class`/`module` body: `def`s become instance methods (or class
    /// methods for `def self.m`), `attr_*` generate accessor methods, `include M`
    /// records a mixin. Any remaining statements (class variables, constants, …)
    /// run once at definition time with `self` bound to the class.
    fn compile_class(
        &mut self,
        b: &mut ChunkBuilder,
        name: &str,
        superclass: &Option<String>,
        body: &[Stmt],
    ) -> Result<(), String> {
        // The class/module is stored under its fully-qualified name (prefixed by
        // the enclosing namespace). The superclass is resolved against the
        // *enclosing* nesting, before this class's own name is pushed.
        let qname = self.qualify(name);
        let resolved_super = superclass.as_ref().map(|s| self.resolve_class_name(s));
        // Push this namespace so nested class/module defs, constant assignments,
        // and bare constant reads inside the body resolve lexically against it.
        self.nesting.push(qname.clone());
        let mut methods: indexmap::IndexMap<String, MethodDef> = indexmap::IndexMap::new();
        let mut class_methods: indexmap::IndexMap<String, MethodDef> = indexmap::IndexMap::new();
        let includes: Vec<String> = Vec::new();
        let mut prepends: Vec<String> = Vec::new();
        let mut extends: Vec<String> = Vec::new();
        // Class-body statements that aren't defs/attrs/includes (e.g. `@@x = 0`,
        // constant assignments) run at class-definition time with `self` bound to
        // the class.
        let mut init_body: Vec<Stmt> = Vec::new();
        // After a bare `module_function` (no args) in a module body, every
        // following `def` becomes both an instance method and a module (class)
        // method — as `Rack::Utils` exposes its helpers.
        let mut module_function_mode = false;
        for stmt in body {
            match &stmt.expr {
                // `def Klass.m` / `def obj.m` inside a class body carries an
                // explicit receiver — defer to runtime (via `__class_body__`).
                Expr::Def {
                    singleton_recv: Some(_),
                    ..
                } => init_body.push(stmt.clone()),
                Expr::Def {
                    name,
                    params,
                    body,
                    singleton,
                    singleton_recv: None,
                } => {
                    let def = self.compile_method_named(name, params, body)?;
                    if *singleton {
                        class_methods.insert(name.clone(), def);
                    } else {
                        if module_function_mode {
                            class_methods.insert(name.clone(), def.clone());
                        }
                        methods.insert(name.clone(), def);
                    }
                }
                // Bare `module_function` (parses as a local read) turns on the mode.
                // Also emit it to the runtime body so the module is flagged for the
                // class-method fallback (covers `def`s the compiler can't promote
                // because they are nested in an `if`/`else`).
                Expr::Var(VarKind::Local, v) if v == "module_function" => {
                    module_function_mode = true;
                    init_body.push(stmt.clone());
                }
                // `module_function :a, :b` promotes the named (already-defined)
                // instance methods to module methods.
                Expr::Call {
                    recv: None,
                    name: m,
                    args,
                    ..
                } if m == "module_function" => {
                    if args.is_empty() {
                        module_function_mode = true;
                        init_body.push(stmt.clone());
                    } else {
                        for a in args {
                            if let Some(field) = sym_name(a) {
                                if let Some(def) = methods.get(&field) {
                                    class_methods.insert(field.clone(), def.clone());
                                }
                            }
                        }
                    }
                }
                Expr::Call {
                    recv: None,
                    name: m,
                    args,
                    ..
                } if matches!(m.as_str(), "attr_accessor" | "attr_reader" | "attr_writer") => {
                    for a in args {
                        if let Some(field) = sym_name(a) {
                            if m != "attr_writer" {
                                methods.insert(field.clone(), self.build_getter(&field));
                            }
                            if m != "attr_reader" {
                                methods.insert(format!("{field}="), self.build_setter(&field));
                            }
                        }
                    }
                }
                // `include ModuleName` / `include A::B` — record the mixin,
                // resolved to the module's qualified registration name.
                Expr::Call {
                    recv: None,
                    name: m,
                    args,
                    ..
                } if m == "include" => {
                    // Run the include inline ONLY (at its lexical position); do NOT
                    // pre-populate the compile-time `includes` list. Building the
                    // ancestry incrementally as each inline `include` executes is what
                    // gives a mid-body `alias :a, :b` the lexically-correct target: an
                    // `alias :_cache_control, :cache_control` between `include
                    // Rack::Response::Helpers` and `include AD::Http::Cache::Response`
                    // must snapshot Rack's `cache_control`, not the later one. A
                    // pre-populated list makes every include visible before any body
                    // statement runs, so the alias would capture the wrong method.
                    init_body.push(stmt.clone());
                }
                // `prepend ModuleName` — record a module that precedes the class.
                Expr::Call {
                    recv: None,
                    name: m,
                    args,
                    ..
                } if m == "prepend" => {
                    for a in args {
                        if let Some(module) = const_path_name(a) {
                            prepends.push(self.resolve_class_name(&module));
                        }
                    }
                    // Run inline too (see the `include` arm) so a concern's
                    // `prepend_features` fires at the statement's position.
                    init_body.push(stmt.clone());
                }
                // `extend ModuleName` — mix the module's instance methods in as
                // class methods.
                Expr::Call {
                    recv: None,
                    name: m,
                    args,
                    ..
                } if m == "extend" => {
                    for a in args {
                        if let Some(module) = const_path_name(a) {
                            extends.push(self.resolve_class_name(&module));
                        } else if matches!(a, Expr::SelfExpr) {
                            // `extend self` — the module gains its own instance
                            // methods as module methods. Register it in its own
                            // extends list (activesupport Inflector, and the
                            // common `module M; extend self; …` singleton idiom).
                            extends.push(qname.clone());
                        }
                    }
                    // Run inline too (see the `include` arm) so the module's
                    // `self.extended` hook fires at the statement's position —
                    // ActiveSupport::Concern.extended sets @_dependencies, which a
                    // later `include AnotherConcern` in the same body checks to
                    // decide whether to defer. Deferring the hook to after the body
                    // would leave @_dependencies unset and run the included block
                    // immediately with the wrong self.
                    init_body.push(stmt.clone());
                }
                // `class << self … end` — its `def`s are class (singleton)
                // methods, the same as `def self.x`.
                Expr::SingletonClass { recv: None, body } => {
                    let mut sc_nondefs: Vec<Stmt> = Vec::new();
                    for s in body {
                        match &s.expr {
                            Expr::Def {
                                name, params, body, ..
                            } => {
                                class_methods
                                    .insert(name.clone(), self.compile_method(params, body)?);
                            }
                            // `attr_accessor`/`attr_reader`/`attr_writer` inside
                            // `class << self` define class-level accessors.
                            Expr::Call {
                                recv: None,
                                name: m,
                                args,
                                ..
                            } if matches!(
                                m.as_str(),
                                "attr_accessor" | "attr_reader" | "attr_writer"
                            ) =>
                            {
                                for a in args {
                                    if let Some(field) = sym_name(a) {
                                        if m != "attr_writer" {
                                            class_methods
                                                .insert(field.clone(), self.build_getter(&field));
                                        }
                                        if m != "attr_reader" {
                                            class_methods.insert(
                                                format!("{field}="),
                                                self.build_setter(&field),
                                            );
                                        }
                                    }
                                }
                            }
                            // Any other directive (`alias_method`, `private`, …)
                            // runs at definition time with `self` = the singleton
                            // class, so it operates on the class's own methods.
                            _ => sc_nondefs.push(s.clone()),
                        }
                    }
                    if !sc_nondefs.is_empty() {
                        // `self.singleton_class.class_eval { <nondefs> }` — deferred
                        // to `init_body`, which runs with `self` = the class.
                        init_body.push(
                            Expr::Call {
                                recv: Some(Box::new(Expr::Call {
                                    recv: Some(Box::new(Expr::SelfExpr)),
                                    name: "singleton_class".into(),
                                    args: Vec::new(),
                                    block: None,
                                })),
                                name: "class_eval".into(),
                                args: Vec::new(),
                                block: Some(Block {
                                    params: Vec::new(),
                                    splat: None,
                                    body: sc_nondefs,
                                    arity: None,
                                }),
                            }
                            .into(),
                        );
                    }
                }
                // `class << obj … end` inside a class body — an explicit receiver;
                // defer to runtime (via `__class_body__`).
                Expr::SingletonClass { recv: Some(_), .. } => init_body.push(stmt.clone()),
                _ => init_body.push(stmt.clone()),
            }
        }
        // Compile the leftover body as a synthetic class method so it can run with
        // `self` = the class. Each class opening (including reopenings of the same
        // name) gets a *unique* body name so a later reopening's `__class_body__`
        // never clobbers an earlier one after the two ClassDefs are merged.
        let body_name = format!("__class_body__{}", next_synth_id());
        if !init_body.is_empty() {
            class_methods.insert(body_name.clone(), self.compile_method(&[], &init_body)?);
        }
        // `include`/`prepend`/`extend` now all run inline (pushed into init_body
        // above), so their `included`/`prepended`/`extended`/append_features hooks
        // fire at the statement's lexical position — deferring them here would
        // double-fire (and, for `extend Concern`, run too late).
        // Done with this namespace: everything referenced below (the class ref
        // to run the body on, hook targets) uses the qualified name, so pop
        // before emitting those so they resolve at the enclosing level.
        self.nesting.pop();
        self.classes.push((
            qname.clone(),
            ClassDef {
                superclass: resolved_super.clone(),
                methods,
                includes,
                prepends,
                extends,
                class_methods,
            },
        ));
        // A named superclass may be an autoloaded constant (`class Sub < Base`
        // where `autoload :Base, …`, as rack-protection does). Read it here so the
        // pending autoload fires and the superclass is fully defined before the
        // subclass body and `inherited` hook run. `const_candidates` resolves it
        // against the enclosing nesting (already popped above).
        if let Some(sc) = superclass {
            let encoded = self.const_candidates(sc);
            self.kstr(b, &encoded);
            b.emit(Op::CallBuiltin(ops::GETCONST, 1), 0);
            b.emit(Op::Pop, 0);
        }
        // `inherited(subclass)` fires when the subclass is opened — before its body.
        if let Some(sc) = &resolved_super {
            self.emit_hook(b, sc, "inherited", &qname);
        }
        // Run the class body (`self` = class) once, at definition time. The class
        // ref is loaded by its qualified name directly (a single-candidate const).
        if !init_body.is_empty() {
            self.kstr(b, &qname);
            b.emit(Op::CallBuiltin(ops::GETCONST, 1), 0);
            self.kstr(b, &body_name);
            b.emit(Op::CallBuiltin(ops::CALL_METHOD, 2), self.cur_line);
            b.emit(Op::Pop, 0);
        }
        // A class/module definition evaluates to nil here.
        b.emit(Op::LoadUndef, 0);
        Ok(())
    }

    /// Emit a runtime `FIRE_HOOK`: if `module` defines the class method `hook`
    /// (`inherited`/`included`/`extended`/`prepended`), call it with a reference
    /// to `target` (the subclass or including class). A no-op if undefined.
    fn emit_hook(&mut self, b: &mut ChunkBuilder, module: &str, hook: &str, target: &str) {
        self.kstr(b, module);
        self.kstr(b, hook);
        self.kstr(b, target);
        b.emit(Op::CallBuiltin(ops::FIRE_HOOK, 3), 0);
        b.emit(Op::Pop, 0);
    }

    /// A generated `attr_reader` method body: `@field`.
    fn build_getter(&mut self, field: &str) -> MethodDef {
        let mut b = ChunkBuilder::new();
        let idx = b.add_constant(Value::Str(Arc::new(field.to_string())));
        b.emit(Op::LoadConst(idx), 0);
        b.emit(Op::CallBuiltin(ops::GETIVAR, 1), 0);
        MethodDef {
            params: vec![],
            splat: None,
            kwparams: vec![],
            kwsplat: None,
            blockparam: None,
            req: 0,
            opt: 0,
            kwreq: vec![],
            chunk: b.build(),
            slot_params: 0,
        }
    }

    /// A generated `attr_writer` method body: `@field = value`.
    fn build_setter(&mut self, field: &str) -> MethodDef {
        let mut b = ChunkBuilder::new();
        let fidx = b.add_constant(Value::Str(Arc::new(field.to_string())));
        b.emit(Op::LoadConst(fidx), 0);
        let vidx = b.add_constant(Value::Str(Arc::new("value".to_string())));
        b.emit(Op::LoadConst(vidx), 0);
        b.emit(Op::CallBuiltin(ops::GETLOCAL, 1), 0);
        b.emit(Op::CallBuiltin(ops::SETIVAR, 2), 0);
        MethodDef {
            params: vec!["value".to_string()],
            splat: None,
            kwparams: vec![],
            kwsplat: None,
            blockparam: None,
            req: 1,
            opt: 0,
            kwreq: vec![],
            chunk: b.build(),
            slot_params: 0,
        }
    }

    fn compile_begin(
        &mut self,
        b: &mut ChunkBuilder,
        body: &[Stmt],
        rescues: &[Rescue],
        ensure: &Option<Vec<Stmt>>,
    ) -> Result<(), String> {
        // Directly inside a native loop, a `redo` in any of these nested chunks is
        // valid: `BEGIN_IN_LOOP` lets the signal survive and the handoff below
        // turns it into a jump to the loop body. `self.loops` is emptied while
        // those chunks compile, so record the fact in the flag instead.
        let saved_redo = self.redo_ok;
        if !self.loops.is_empty() {
            self.redo_ok = true;
        }
        let body_id = self.compile_proc_body(body, &[], None)?;
        let mut rdefs = Vec::new();
        for r in rescues {
            let params: Vec<String> = r.binding.iter().cloned().collect();
            let rid = self.compile_proc_body(&r.body, &params, None)?;
            // A `rescue *expr` splat compiles to a zero-arg proc returning the
            // class (or array of classes) to match at runtime.
            let splat = match &r.splat {
                Some(e) => Some(self.compile_proc_body(
                    std::slice::from_ref(&Stmt::from(e.clone())),
                    &[],
                    None,
                )?),
                None => None,
            };
            rdefs.push(RescueDef {
                classes: r.classes.clone(),
                splat,
                binding: r.binding.clone(),
                body: rid,
            });
        }
        let ensure_id = match ensure {
            Some(e) => Some(self.compile_proc_body(e, &[], None)?),
            None => None,
        };
        self.redo_ok = saved_redo;
        let begin_id = self.begins.len();
        self.begins.push(BeginDef {
            body: body_id,
            rescues: rdefs,
            ensure: ensure_id,
        });
        b.emit(Op::LoadInt(begin_id as i64), 0);
        // Directly inside a native loop, use the variant that lets a `break`/
        // `next` signal survive to the handoff ops below instead of unwinding.
        let op = if self.loops.is_empty() {
            ops::BEGIN
        } else {
            ops::BEGIN_IN_LOOP
        };
        b.emit(Op::CallBuiltin(op, 1), 0);
        self.emit_loop_signal_handoff(b);
        Ok(())
    }

    /// Re-dispatch a `break`/`next` that a nested chunk could only signal.
    ///
    /// A `begin` body is compiled into its own chunk, so a `break` inside it
    /// cannot jump to the enclosing native `while`/`until`/`for` exit — that
    /// label lives in *this* chunk. It raises a signal instead, and the nested
    /// run unwinds to here with the signal still pending. This turns it back
    /// into the jump the loop was expecting.
    ///
    /// Emitted only when a native loop is actually open: inside a block body the
    /// loop stack is empty, and a `break` there belongs to the method being
    /// called, so it must keep unwinding untouched.
    fn emit_loop_signal_handoff(&mut self, b: &mut ChunkBuilder) {
        if self.loops.is_empty() {
            return;
        }
        // Stack on entry is `[value]`, and every path leaves it that way except
        // the two that jump out (which drop it first, as a `break`/`next`
        // compiled in this chunk would).
        b.emit(Op::CallBuiltin(ops::TAKE_LOOP_NEXT, 0), 0);
        let no_next = b.emit(Op::JumpIfFalse(0), 0);
        b.emit(Op::Pop, 0);
        let next_jump = b.emit(Op::Jump(0), 0);
        b.patch_jump(no_next, b.current_pos());

        // Peek first, consume only on the branch that jumps: a `break` this loop
        // does not own has to stay pending.
        b.emit(Op::CallBuiltin(ops::PEEK_LOOP_BREAK, 0), 0);
        let no_break = b.emit(Op::JumpIfFalse(0), 0);
        b.emit(Op::Pop, 0);
        b.emit(Op::CallBuiltin(ops::TAKE_LOOP_BREAK, 0), 0);
        let break_jump = b.emit(Op::Jump(0), 0);
        b.patch_jump(no_break, b.current_pos());

        // A `redo` raised inside the nested chunk re-runs this loop's body.
        b.emit(Op::CallBuiltin(ops::TAKE_LOOP_REDO, 0), 0);
        let no_redo = b.emit(Op::JumpIfFalse(0), 0);
        b.emit(Op::Pop, 0);
        let redo_jump = b.emit(Op::Jump(0), 0);
        b.patch_jump(no_redo, b.current_pos());

        let ctx = self.loops.last_mut().unwrap();
        ctx.nexts.push(next_jump);
        ctx.breaks.push(break_jump);
        ctx.redos.push(redo_jump);
    }

    fn compile_flow(
        &mut self,
        b: &mut ChunkBuilder,
        e: &Option<Box<Expr>>,
        sig_op: u16,
        kind: FlowKind,
    ) -> Result<(), String> {
        // Inside a native `while` loop, break/next are jumps; a return, or any
        // of them inside a block body (its own chunk, no loop ctx), is a signal.
        let in_loop = !self.loops.is_empty();
        match e {
            Some(e) => self.compile_expr(b, e)?,
            None => {
                b.emit(Op::LoadUndef, 0);
            }
        }
        if in_loop && matches!(kind, FlowKind::Break | FlowKind::Next) {
            // `break VALUE` makes that value the loop's own value, so it stays on
            // the stack and lands at the loop's exit label. `next` discards it —
            // a native loop's per-iteration value is thrown away regardless.
            if matches!(kind, FlowKind::Next) {
                b.emit(Op::Pop, 0);
            }
            let j = b.emit(Op::Jump(0), 0);
            let ctx = self.loops.last_mut().unwrap();
            match kind {
                FlowKind::Break => ctx.breaks.push(j),
                FlowKind::Next => ctx.nexts.push(j),
                _ => {}
            }
            b.emit(Op::LoadUndef, 0); // leave a value (dead if jump taken)
        } else {
            b.emit(Op::CallBuiltin(sig_op, 1), 0);
        }
        Ok(())
    }

    /// Emit a native string constant load.
    fn kstr(&mut self, b: &mut ChunkBuilder, s: &str) {
        let idx = b.add_constant(Value::Str(Arc::new(s.to_string())));
        b.emit(Op::LoadConst(idx), 0);
    }

    /// Lower `defined?(operand)`. Literals map to a fixed description; a variable
    /// or bare name defers to the `DEFINED_DESC` runtime check (which returns
    /// `nil` when the thing is not defined); other shapes get a static
    /// classification (`"method"`/`"assignment"`/`"expression"`). The operand is
    /// never evaluated.
    fn compile_defined(&mut self, b: &mut ChunkBuilder, operand: &Expr) -> Result<(), String> {
        // Push a fixed heap-String description.
        let lit = |c: &mut Self, b: &mut ChunkBuilder, s: &str| {
            c.kstr(b, s);
            b.emit(Op::CallBuiltin(ops::MKSTR, 1), 0);
        };
        // Emit a runtime `DEFINED_DESC(kind, name)` check.
        let check = |c: &mut Self, b: &mut ChunkBuilder, kind: &str, name: &str| {
            c.kstr(b, kind);
            c.kstr(b, name);
            b.emit(Op::CallBuiltin(ops::DEFINED_DESC, 2), 0);
        };
        match operand {
            Expr::Nil => lit(self, b, "nil"),
            Expr::True => lit(self, b, "true"),
            Expr::False => lit(self, b, "false"),
            Expr::SelfExpr => lit(self, b, "self"),
            Expr::Assign(..) | Expr::MultiAssign { .. } => lit(self, b, "assignment"),
            Expr::Var(VarKind::Const, n) => check(self, b, "const", n),
            Expr::Var(VarKind::Instance, n) => check(self, b, "ivar", n),
            Expr::Var(VarKind::Global, n) => check(self, b, "gvar", n),
            Expr::Var(VarKind::Class, n) => check(self, b, "cvar", n),
            Expr::Var(VarKind::Local, n) => check(self, b, "local", n),
            Expr::Yield(_) => check(self, b, "yield", ""),
            // A qualified constant path (`A::B`, `URI::RFC2396_PARSER`) parses as a
            // no-arg capitalized method call on a constant path. `defined?` must
            // treat it as a constant reference — returning nil (not raising) when
            // the constant is absent — rather than reporting "method".
            Expr::Call { .. } if const_path_name(operand).is_some() => {
                let path = const_path_name(operand).unwrap();
                check(self, b, "const", &path);
            }
            // A bare name with no receiver/args is a local-or-method reference.
            Expr::Call {
                recv: None,
                name,
                args,
                block: None,
            } if args.is_empty() => check(self, b, "local", name),
            // Any other call (with a receiver or args) or an operator is a method.
            Expr::Call { .. } | Expr::Binary(..) | Expr::Unary(..) | Expr::Index(..) => {
                lit(self, b, "method")
            }
            _ => lit(self, b, "expression"),
        }
        Ok(())
    }
}

enum FlowKind {
    Return,
    Break,
    Next,
}

/// The field name from an `attr_*` argument (`:sym` or `"str"`).
/// If `block` is a synthetic pass-through (`&expr` / `...` forwarding) — no
/// params and a single `BlockPass` body statement — return its inner value
/// expression, which `compile_call` emits as the block operand directly.
fn block_pass_expr(block: &Block) -> Option<&Expr> {
    match block.body.as_slice() {
        [stmt] => match &stmt.expr {
            Expr::BlockPass(e) => Some(e),
            _ => None,
        },
        _ => None,
    }
}

fn sym_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Symbol(s) => Some(s.clone()),
        Expr::Str(parts) => match parts.as_slice() {
            [StrPart::Lit(s)] => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Flatten a constant reference used as a mixin argument into its written path:
/// a bare `Expr::Var(Const, "M")` → `"M"`, and a `::`-chain (which parses as a
/// no-arg capitalized method call, `A::B` → `Call{recv: A, name: "B"}`) →
/// `"A::B"`. Returns `None` for anything that is not a constant path.
fn const_path_name(e: &Expr) -> Option<String> {
    match e {
        // Strip a leading `::` marker (top-level scope) — rubylang's constants are
        // effectively flat, so `::Foo` resolves as `Foo`.
        Expr::Var(VarKind::Const, n) => Some(n.strip_prefix("::").unwrap_or(n).to_string()),
        Expr::Call {
            recv: Some(r),
            name,
            args,
            block,
        } if args.is_empty()
            && block.is_none()
            && name.chars().next().is_some_and(|c| c.is_uppercase()) =>
        {
            Some(format!("{}::{}", const_path_name(r)?, name))
        }
        _ => None,
    }
}

/// argc must fit in a `u8`; a call/collection wider than 255 is rejected.
fn argc(n: usize) -> Result<u8, String> {
    u8::try_from(n).map_err(|_| format!("too many arguments/elements ({n} > 255)"))
}

// ===========================================================================
// Pattern-match lowering (`case/in`).
// ===========================================================================

/// A method call `recv.name(args)`.
fn pcall(recv: Expr, name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call {
        recv: Some(Box::new(recv)),
        name: name.to_string(),
        args,
        block: None,
    }
}

fn pand(a: Expr, b: Expr) -> Expr {
    Expr::Binary(BinOp::And, Box::new(a), Box::new(b))
}

/// The element `subj[i]` (a negative `i` counts from the end).
fn pindex(subj: &Expr, i: i64) -> Expr {
    Expr::Index(Box::new(subj.clone()), vec![Expr::Int(i)])
}

/// `subj[idx]` with a runtime index expression.
fn pindex_expr(subj: &Expr, idx: Expr) -> Expr {
    Expr::Index(Box::new(subj.clone()), vec![idx])
}

/// `e + n` / `e - n` as fusevm arithmetic.
fn padd(e: Expr, n: i64) -> Expr {
    Expr::Binary(BinOp::Add, Box::new(e), Box::new(Expr::Int(n)))
}
fn psub(e: Expr, n: i64) -> Expr {
    Expr::Binary(BinOp::Sub, Box::new(e), Box::new(Expr::Int(n)))
}

/// Unique-name source for find-pattern scan temporaries.
static FIND_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Lower a pattern against a subject expression into `(test, bindings)`: a
/// boolean test expression, and the assignments to run when the shape matches
/// (before any guard). Bindings are plain `Expr::Assign`s to local variables.
fn lower_pattern(pat: &Pattern, subj: &Expr) -> (Expr, Vec<Expr>) {
    match pat {
        // `in 5` / `in 1..10` / `in Integer` — case-equality (`pattern === subj`).
        Pattern::Value(v) => (pcall(v.clone(), "===", vec![subj.clone()]), vec![]),
        // `in ^x` — pinned value, matched with `==`.
        Pattern::Pin(e) => (
            Expr::Binary(BinOp::Eq, Box::new(subj.clone()), Box::new(e.clone())),
            vec![],
        ),
        Pattern::Bind(name) if name == "_" => (Expr::True, vec![]),
        Pattern::Bind(name) => (
            Expr::True,
            vec![Expr::Assign(
                Box::new(Expr::Var(VarKind::Local, name.clone())),
                Box::new(subj.clone()),
            )],
        ),
        Pattern::Const(name, None) => (
            pcall(
                Expr::Var(VarKind::Const, name.clone()),
                "===",
                vec![subj.clone()],
            ),
            vec![],
        ),
        // `Const[...]` / `Const(...)` — class check plus a deconstructed match.
        Pattern::Const(name, Some(inner)) => {
            let type_test = pcall(
                Expr::Var(VarKind::Const, name.clone()),
                "===",
                vec![subj.clone()],
            );
            let decon = pcall(subj.clone(), "deconstruct", vec![]);
            let (t, binds) = lower_pattern(inner, &decon);
            (pand(type_test, t), binds)
        }
        // `pat => name` — bind the whole subject after the inner pattern matches.
        Pattern::As(inner, name) => {
            let (t, mut binds) = lower_pattern(inner, subj);
            binds.push(Expr::Assign(
                Box::new(Expr::Var(VarKind::Local, name.clone())),
                Box::new(subj.clone()),
            ));
            (t, binds)
        }
        // `p1 | p2 | …` — any alternative matches (bindings inside are ignored).
        Pattern::Or(alts) => {
            let test = alts
                .iter()
                .map(|p| lower_pattern(p, subj).0)
                .reduce(|a, c| Expr::Binary(BinOp::Or, Box::new(a), Box::new(c)))
                .unwrap_or(Expr::False);
            (test, vec![])
        }
        Pattern::Splat(_) => (Expr::True, vec![]), // handled inside Array
        Pattern::Array(elems) => lower_array_pattern(elems, subj),
        Pattern::Hash(pairs, rest) => lower_hash_pattern(pairs, rest, subj),
    }
}

fn lower_array_pattern(elems: &[Pattern], subj: &Expr) -> (Expr, Vec<Expr>) {
    // Two-sided find pattern `[*pre, mid…, *post]`: leading and trailing splats
    // with ≥1 non-splat between them and no other splats.
    if elems.len() >= 3
        && matches!(elems.first(), Some(Pattern::Splat(_)))
        && matches!(elems.last(), Some(Pattern::Splat(_)))
        && !elems[1..elems.len() - 1]
            .iter()
            .any(|p| matches!(p, Pattern::Splat(_)))
    {
        return lower_find_pattern(elems, subj);
    }
    // Array patterns match any object responding to `deconstruct` (Arrays return
    // self). The `respond_to?` gate short-circuits, so `deconstruct` runs only on
    // a matching receiver; its result is bound once to `d`, and every length /
    // index below reads `d`, never the raw subject.
    let uid = FIND_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = Expr::Var(VarKind::Local, format!("__decon{uid}__"));
    let gate = pcall(
        subj.clone(),
        "respond_to?",
        vec![Expr::Symbol("deconstruct".into())],
    );
    let assign_d = Expr::Assign(
        Box::new(d.clone()),
        Box::new(pcall(subj.clone(), "deconstruct", vec![])),
    );
    let splat_at = elems.iter().position(|p| matches!(p, Pattern::Splat(_)));
    let len = || pcall(d.clone(), "length", vec![]);

    let mut test = pand(gate, assign_d);
    let mut binds = Vec::new();
    match splat_at {
        None => {
            // Exact length, then element-wise.
            test = pand(
                test,
                Expr::Binary(
                    BinOp::Eq,
                    Box::new(len()),
                    Box::new(Expr::Int(elems.len() as i64)),
                ),
            );
            for (i, p) in elems.iter().enumerate() {
                let (t, bs) = lower_pattern(p, &pindex(&d, i as i64));
                test = pand(test, t);
                binds.extend(bs);
            }
        }
        Some(s) => {
            let pre = &elems[..s];
            let post = &elems[s + 1..];
            let min = (pre.len() + post.len()) as i64;
            test = pand(
                test,
                Expr::Binary(BinOp::Ge, Box::new(len()), Box::new(Expr::Int(min))),
            );
            for (i, p) in pre.iter().enumerate() {
                let (t, bs) = lower_pattern(p, &pindex(&d, i as i64));
                test = pand(test, t);
                binds.extend(bs);
            }
            // The `*rest` slice: `d[pre_len, length - pre_len - post_len]`.
            if let Pattern::Splat(Some(name)) = &elems[s] {
                let count = Expr::Binary(
                    BinOp::Sub,
                    Box::new(Expr::Binary(
                        BinOp::Sub,
                        Box::new(len()),
                        Box::new(Expr::Int(pre.len() as i64)),
                    )),
                    Box::new(Expr::Int(post.len() as i64)),
                );
                let slice = Expr::Index(
                    Box::new(d.clone()),
                    vec![Expr::Int(pre.len() as i64), count],
                );
                binds.push(Expr::Assign(
                    Box::new(Expr::Var(VarKind::Local, name.clone())),
                    Box::new(slice),
                ));
            }
            // Post-splat elements count back from the end.
            for (j, p) in post.iter().enumerate() {
                let idx = -(post.len() as i64) + j as i64;
                let (t, bs) = lower_pattern(p, &pindex(&d, idx));
                test = pand(test, t);
                binds.extend(bs);
            }
        }
    }
    (test, binds)
}

/// Lower a two-sided find pattern `[*pre, mid…, *post]`. Scan for the first
/// start index where every middle pattern matches, then bind `pre`/`post` to
/// the surrounding slices and re-run the middle bindings at that index.
fn lower_find_pattern(elems: &[Pattern], subj: &Expr) -> (Expr, Vec<Expr>) {
    let pre = &elems[0];
    let post = &elems[elems.len() - 1];
    let mids = &elems[1..elems.len() - 1];
    let k = mids.len() as i64;

    let uid = FIND_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The subject is deconstructed once into `d`; the scan and slices read `d`.
    let d = Expr::Var(VarKind::Local, format!("__findd{uid}__"));
    let idx = Expr::Var(VarKind::Local, format!("__find_i{uid}__"));
    let s_name = format!("__find_s{uid}__");
    let s = Expr::Var(VarKind::Local, s_name.clone());
    let len = || pcall(d.clone(), "length", vec![]);

    // Predicate over a candidate start `s`: every middle matches at `d[s+j]`.
    let pred = mids
        .iter()
        .enumerate()
        .map(|(j, m)| lower_pattern(m, &pindex_expr(&d, padd(s.clone(), j as i64))).0)
        .reduce(pand)
        .unwrap_or(Expr::True);

    // idx = nil
    // (0..(len - k)).each { |s| idx = s if idx.nil? && pred }
    // idx != nil
    let scan = Expr::If {
        cond: Box::new(pand(pcall(idx.clone(), "nil?", vec![]), pred)),
        then: vec![Expr::Assign(Box::new(idx.clone()), Box::new(s.clone())).into()],
        elifs: vec![],
        els: None,
    };
    let range = Expr::Range {
        lo: Some(Box::new(Expr::Int(0))),
        hi: Some(Box::new(psub(len(), k))),
        exclusive: false,
    };
    let each = Expr::Call {
        recv: Some(Box::new(range)),
        name: "each".into(),
        args: vec![],
        block: Some(Block {
            params: vec![s_name],
            splat: None,
            body: vec![scan.into()],
            arity: None,
        }),
    };
    let found = Expr::Begin {
        body: vec![
            Expr::Assign(Box::new(idx.clone()), Box::new(Expr::Nil)).into(),
            each.into(),
            Expr::Binary(BinOp::Ne, Box::new(idx.clone()), Box::new(Expr::Nil)).into(),
        ],
        rescues: vec![],
        ensure: None,
    };
    // Find patterns match any object responding to `deconstruct`; the gate
    // short-circuits and the deconstructed array is bound once to `d` before the
    // scan runs.
    let gate = pcall(
        subj.clone(),
        "respond_to?",
        vec![Expr::Symbol("deconstruct".into())],
    );
    let assign_d = Expr::Assign(
        Box::new(d.clone()),
        Box::new(pcall(subj.clone(), "deconstruct", vec![])),
    );
    let test = pand(gate, pand(assign_d, found));

    // Bindings run once `idx` holds the first matching start position.
    let mut binds = Vec::new();
    if let Pattern::Splat(Some(name)) = pre {
        binds.push(Expr::Assign(
            Box::new(Expr::Var(VarKind::Local, name.clone())),
            Box::new(Expr::Index(
                Box::new(d.clone()),
                vec![Expr::Int(0), idx.clone()],
            )),
        ));
    }
    for (j, m) in mids.iter().enumerate() {
        binds.extend(lower_pattern(m, &pindex_expr(&d, padd(idx.clone(), j as i64))).1);
    }
    if let Pattern::Splat(Some(name)) = post {
        let start = padd(idx.clone(), k);
        let count = Expr::Binary(BinOp::Sub, Box::new(len()), Box::new(start.clone()));
        binds.push(Expr::Assign(
            Box::new(Expr::Var(VarKind::Local, name.clone())),
            Box::new(Expr::Index(Box::new(d.clone()), vec![start, count])),
        ));
    }
    (test, binds)
}

fn lower_hash_pattern(
    pairs: &[(String, Option<Pattern>)],
    rest: &HashRest,
    subj: &Expr,
) -> (Expr, Vec<Expr>) {
    // Hash patterns match any object responding to `deconstruct_keys` (Hashes
    // return self). MRI passes the array of requested symbol keys, or `nil` when
    // the pattern is open (`**rest`), `**nil`, or empty (`{}`). The gate
    // short-circuits so `deconstruct_keys` runs only on a matching receiver; its
    // result is bound once to `dh` and every lookup below reads `dh`.
    let uid = FIND_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dh = Expr::Var(VarKind::Local, format!("__deconk{uid}__"));
    let keys_arg = if matches!(rest, HashRest::None) && !pairs.is_empty() {
        Expr::Array(pairs.iter().map(|(k, _)| Expr::Symbol(k.clone())).collect())
    } else {
        Expr::Nil
    };
    let gate = pcall(
        subj.clone(),
        "respond_to?",
        vec![Expr::Symbol("deconstruct_keys".into())],
    );
    let assign_dh = Expr::Assign(
        Box::new(dh.clone()),
        Box::new(pcall(subj.clone(), "deconstruct_keys", vec![keys_arg])),
    );
    let mut test = pand(gate, assign_dh);
    let mut binds = Vec::new();
    for (key, sub) in pairs {
        let at = Expr::Index(Box::new(dh.clone()), vec![Expr::Symbol(key.clone())]);
        test = pand(
            test,
            pcall(dh.clone(), "key?", vec![Expr::Symbol(key.clone())]),
        );
        match sub {
            None => binds.push(Expr::Assign(
                Box::new(Expr::Var(VarKind::Local, key.clone())),
                Box::new(at),
            )),
            Some(p) => {
                let (t, bs) = lower_pattern(p, &at);
                test = pand(test, t);
                binds.extend(bs);
            }
        }
    }
    // `**name` binds the remaining keys (those not named in the pattern).
    if let HashRest::Splat(Some(name)) = rest {
        let except_args = pairs
            .iter()
            .map(|(k, _)| Expr::Symbol(k.clone()))
            .collect::<Vec<_>>();
        binds.push(Expr::Assign(
            Box::new(Expr::Var(VarKind::Local, name.clone())),
            Box::new(pcall(dh.clone(), "except", except_args)),
        ));
    }
    // `**nil` (or a bare `{}`) forbids any other keys: the subject must contain
    // exactly the listed keys.
    let exact =
        matches!(rest, HashRest::Nil) || (matches!(rest, HashRest::None) && pairs.is_empty());
    if exact {
        test = pand(
            test,
            Expr::Binary(
                BinOp::Eq,
                Box::new(pcall(dh.clone(), "length", vec![])),
                Box::new(Expr::Int(pairs.len() as i64)),
            ),
        );
    }
    (test, binds)
}
