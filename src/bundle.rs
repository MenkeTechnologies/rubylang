//! Build-time `require` bundling — the AOT whole-app merge.
//!
//! `ruby --build FILE` compiles the entrypoint *and everything it statically
//! requires* into one program, so a multi-file app AOT-compiles as a single
//! unit with no runtime file I/O. This module does the static resolution: it
//! walks the entrypoint's AST, and for every `require_relative "..."` /
//! `require "..."` whose argument is a **literal** string, it resolves the path
//! with the same logic the runtime uses (`builtins::resolve_in` /
//! `resolve_require_in`), reads + parses the target, recursively bundles *its*
//! requires, and splices the result in **at the require site** — wrapped in a
//! `begin … end` so evaluation order matches MRI: the required file's top level
//! runs exactly where the `require` sat, after the requires that precede it.
//!
//! Each absolute path is bundled once. A second `require` of an already-bundled
//! path becomes `false` (MRI's already-loaded return value). Anything that can't
//! be resolved at build time is left untouched as a runtime call, so nothing
//! that works today regresses:
//!   * a builtin-lib name (`json`, `socket`, …) stays a runtime no-op;
//!   * a computed / interpolated (non-literal) argument stays a runtime require;
//!   * a literal path that does not resolve stays a runtime require (and raises
//!     `LoadError` at run time, exactly as MRI would).
//!
//! Requires inside a `def` / block / lambda body are **not** bundled: those run
//! when the method is *called*, not at load time, so inlining them would change
//! semantics. They are left as runtime calls. Class/module/`begin`/`if` bodies
//! *are* load-time, so their literal requires are bundled.
//!
//! The combined `Vec<Stmt>` that `bundle` returns lowers through the normal
//! `compiler::compile` path as a single program.

use crate::ast::{Expr, Stmt, StrPart};
use crate::builtins::{is_builtin_lib, resolve_in, resolve_require_in};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What a build-time bundle produced (for the `--build` report).
pub struct BundleReport {
    /// Absolute paths of every file inlined (entrypoint first), in dedup order.
    pub files: Vec<PathBuf>,
    /// `require`/`require_relative` calls left as runtime calls: builtin libs,
    /// non-literal arguments, and literal paths that did not resolve.
    pub dynamic: usize,
}

/// Parse `entry`, recursively inline every statically resolvable literal
/// `require`/`require_relative`, and return the combined statement list plus a
/// report. The entry file itself is recorded first so a self-require dedups.
pub fn bundle(entry: &Path) -> Result<(Vec<Stmt>, BundleReport), String> {
    let abs = std::fs::canonicalize(entry)
        .map_err(|e| format!("cannot read {}: {e}", entry.display()))?;
    let dir = abs
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    // Build-time `$LOAD_PATH` seed: the entrypoint's own directory (MRI seeds the
    // running script's dir) plus the current working directory.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut load_dirs = vec![dir.clone()];
    if cwd != dir {
        load_dirs.push(cwd);
    }
    let mut stmts = crate::parser::parse(&read(&abs)?)?;
    let mut b = Bundler {
        seen: HashSet::new(),
        files: vec![abs.clone()],
        dynamic: 0,
        load_dirs,
    };
    b.seen.insert(abs);
    b.walk_body(&mut stmts, &dir)?;
    Ok((
        stmts,
        BundleReport {
            files: b.files,
            dynamic: b.dynamic,
        },
    ))
}

struct Bundler {
    /// Absolute paths already inlined — dedup + cycle break.
    seen: HashSet<PathBuf>,
    /// Inlined paths in order (entry first) for the report and the dep manifest.
    files: Vec<PathBuf>,
    /// Requires left as runtime calls.
    dynamic: usize,
    /// Search roots for a plain `require` (entry dir + cwd).
    load_dirs: Vec<PathBuf>,
}

impl Bundler {
    /// Walk a statement body, transforming each expression against `base` (the
    /// directory of the file the statements came from — the base for
    /// `require_relative`).
    fn walk_body(&mut self, body: &mut [Stmt], base: &Path) -> Result<(), String> {
        for s in body {
            self.walk_expr(&mut s.expr, base)?;
        }
        Ok(())
    }

    /// Transform one expression in place: bundle it if it is a resolvable literal
    /// require, else recurse into its load-time children.
    fn walk_expr(&mut self, e: &mut Expr, base: &Path) -> Result<(), String> {
        if self.try_require(e, base)? {
            return Ok(());
        }
        match e {
            Expr::Str(parts) => {
                for p in parts {
                    if let StrPart::Interp(x) = p {
                        self.walk_expr(x, base)?;
                    }
                }
            }
            Expr::Array(xs) => self.walk_exprs(xs, base)?,
            Expr::Hash(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k, base)?;
                    self.walk_expr(v, base)?;
                }
            }
            Expr::Range { lo, hi, .. } => {
                self.walk_opt(lo, base)?;
                self.walk_opt(hi, base)?;
            }
            Expr::Assign(t, v) => {
                self.walk_expr(t, base)?;
                self.walk_expr(v, base)?;
            }
            Expr::MultiAssign { targets, values } => {
                self.walk_exprs(targets, base)?;
                self.walk_exprs(values, base)?;
            }
            Expr::Unary(_, x) => self.walk_expr(x, base)?,
            Expr::Binary(_, a, b) => {
                self.walk_expr(a, base)?;
                self.walk_expr(b, base)?;
            }
            Expr::If {
                cond,
                then,
                elifs,
                els,
            } => {
                self.walk_expr(cond, base)?;
                self.walk_body(then, base)?;
                for (c, body) in elifs {
                    self.walk_expr(c, base)?;
                    self.walk_body(body, base)?;
                }
                if let Some(body) = els {
                    self.walk_body(body, base)?;
                }
            }
            Expr::While { cond, body } | Expr::DoWhile { cond, body } => {
                self.walk_expr(cond, base)?;
                self.walk_body(body, base)?;
            }
            Expr::For { iter, body, .. } => {
                self.walk_expr(iter, base)?;
                self.walk_body(body, base)?;
            }
            Expr::Case {
                subject,
                whens,
                els,
            } => {
                self.walk_expr(subject, base)?;
                for (conds, body) in whens {
                    self.walk_exprs(conds, base)?;
                    self.walk_body(body, base)?;
                }
                if let Some(body) = els {
                    self.walk_body(body, base)?;
                }
            }
            Expr::CaseIn {
                subject,
                clauses,
                els,
            } => {
                self.walk_expr(subject, base)?;
                for cl in clauses {
                    if let Some(g) = &mut cl.guard {
                        self.walk_expr(g, base)?;
                    }
                    self.walk_body(&mut cl.body, base)?;
                }
                if let Some(body) = els {
                    self.walk_body(body, base)?;
                }
            }
            // A non-require call: recurse the receiver and arguments (a require
            // nested as an argument is still found), but not the block body —
            // a block runs when the method yields, not at load time.
            Expr::Call { recv, args, .. } => {
                self.walk_opt(recv, base)?;
                self.walk_exprs(args, base)?;
            }
            Expr::Index(recv, idxs) => {
                self.walk_expr(recv, base)?;
                self.walk_exprs(idxs, base)?;
            }
            // Class/module bodies execute at load time — bundle their requires.
            Expr::Class { body, .. } | Expr::Module { body, .. } => self.walk_body(body, base)?,
            Expr::SingletonClass { body, .. } => self.walk_body(body, base)?,
            Expr::Begin {
                body,
                rescues,
                ensure,
            } => {
                self.walk_body(body, base)?;
                for r in rescues {
                    self.walk_body(&mut r.body, base)?;
                }
                if let Some(body) = ensure {
                    self.walk_body(body, base)?;
                }
            }
            Expr::Return(x) | Expr::Break(x) | Expr::Next(x) => self.walk_opt(x, base)?,
            Expr::Yield(xs) => self.walk_exprs(xs, base)?,
            Expr::Super { args: Some(xs), .. } => self.walk_exprs(xs, base)?,
            Expr::Splat(x) => self.walk_expr(x, base)?,
            Expr::Defined(x) => self.walk_expr(x, base)?,
            // `def` / lambda bodies run when invoked, not at load time — their
            // requires stay runtime calls. Everything else is a leaf.
            _ => {}
        }
        Ok(())
    }

    fn walk_exprs(&mut self, xs: &mut [Expr], base: &Path) -> Result<(), String> {
        for x in xs {
            self.walk_expr(x, base)?;
        }
        Ok(())
    }

    fn walk_opt(&mut self, x: &mut Option<Box<Expr>>, base: &Path) -> Result<(), String> {
        if let Some(x) = x {
            self.walk_expr(x, base)?;
        }
        Ok(())
    }

    /// If `e` is a resolvable literal `require`/`require_relative`, replace it in
    /// place with the required file's inlined body (`begin … end`) — or `false`
    /// if that file is already bundled — and return `Ok(true)`. Otherwise leave
    /// `e` untouched and return `Ok(false)`.
    fn try_require(&mut self, e: &mut Expr, base: &Path) -> Result<bool, String> {
        let (relative, raw) = match e {
            Expr::Call {
                recv: None,
                name,
                args,
                block: None,
            } => {
                let relative = match name.as_str() {
                    "require" => false,
                    "require_relative" => true,
                    _ => return Ok(false),
                };
                match args.as_slice() {
                    [arg] => match literal_str(arg) {
                        Some(s) => (relative, s),
                        // Computed argument: can't resolve at build time.
                        None => {
                            self.dynamic += 1;
                            return Ok(false);
                        }
                    },
                    // Not the 1-arg form: leave it alone.
                    _ => return Ok(false),
                }
            }
            _ => return Ok(false),
        };

        // A builtin lib is a runtime no-op — never bundled.
        if !relative && is_builtin_lib(&raw) {
            self.dynamic += 1;
            return Ok(false);
        }

        let resolved = if relative {
            resolve_in(base, &raw)
        } else {
            resolve_require_in(&self.load_dirs, &raw)
        };
        let Some(target) = resolved else {
            // Unresolvable literal path: leave it so the runtime raises LoadError
            // exactly as MRI would.
            self.dynamic += 1;
            return Ok(false);
        };

        if self.seen.contains(&target) {
            *e = Expr::False; // already bundled -> MRI's second-require value
            return Ok(true);
        }
        self.seen.insert(target.clone());
        self.files.push(target.clone());

        let src = read(&target)?;
        let mut stmts = crate::parser::parse(&src)?;
        let child_dir = target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.walk_body(&mut stmts, &child_dir)?;
        // Inline the required file's top level right here. `begin … end` keeps its
        // top-level locals from leaking into the requiring file's scope, while
        // its `def`/`class`/constant definitions are hoisted globally by the
        // compiler regardless of nesting.
        *e = Expr::Begin {
            body: stmts,
            rescues: vec![],
            ensure: None,
        };
        Ok(true)
    }
}

/// Extract a purely-literal string from a `Str` expression (no interpolation).
fn literal_str(e: &Expr) -> Option<String> {
    match e {
        Expr::Str(parts) => {
            let mut s = String::new();
            for p in parts {
                match p {
                    StrPart::Lit(t) => s.push_str(t),
                    StrPart::Interp(_) => return None,
                }
            }
            Some(s)
        }
        _ => None,
    }
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))
}
