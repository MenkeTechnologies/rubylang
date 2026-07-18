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

/// The full output of compiling a program.
pub struct Program {
    pub main: Chunk,
    pub methods: Vec<(String, MethodDef)>,
    pub classes: Vec<(String, ClassDef)>,
    pub begins: Vec<BeginDef>,
    pub procs: Vec<ProcDef>,
}

/// Break/next jump fixups for a native `while` loop.
struct LoopCtx {
    start: usize,
    breaks: Vec<usize>,
    nexts: Vec<usize>,
}

#[derive(Default)]
pub struct Compiler {
    methods: Vec<(String, MethodDef)>,
    classes: Vec<(String, ClassDef)>,
    begins: Vec<BeginDef>,
    procs: Vec<ProcDef>,
    loops: Vec<LoopCtx>,
}

/// Compile a parsed program.
pub fn compile(stmts: &[Stmt]) -> Result<Program, String> {
    let mut c = Compiler::default();
    let mut b = ChunkBuilder::new();
    c.compile_seq(
        &mut b,
        &stmts.iter().map(|s| s.expr.clone()).collect::<Vec<_>>(),
    )?;
    Ok(Program {
        main: b.build(),
        methods: c.methods,
        classes: c.classes,
        begins: c.begins,
        procs: c.procs,
    })
}

impl Compiler {
    /// Compile a sequence of expressions as a body: each value but the last is
    /// popped; the last is left on the stack (Ruby's "value is the last expr").
    fn compile_seq(&mut self, b: &mut ChunkBuilder, body: &[Expr]) -> Result<(), String> {
        if body.is_empty() {
            b.emit(Op::LoadUndef, 0);
            return Ok(());
        }
        for (i, e) in body.iter().enumerate() {
            self.compile_expr(b, e)?;
            if i + 1 < body.len() {
                b.emit(Op::Pop, 0);
            }
        }
        Ok(())
    }

    /// Compile a method: a prologue that fills in any defaulted parameter the
    /// caller omitted (`defined?(p) ? p : <default>`), then the body.
    fn compile_method(&mut self, params: &[Param], body: &[Expr]) -> Result<MethodDef, String> {
        let saved = std::mem::take(&mut self.loops);
        let mut b = ChunkBuilder::new();
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
        self.loops = saved;
        // Positional params (name-bound by index) are separate from keyword
        // params (bound from the trailing keyword hash).
        let pnames: Vec<String> = params
            .iter()
            .filter(|p| !p.keyword)
            .map(|p| p.name.clone())
            .collect();
        let kwparams: Vec<String> = params
            .iter()
            .filter(|p| p.keyword)
            .map(|p| p.name.clone())
            .collect();
        let splat = params.iter().filter(|p| !p.keyword).position(|p| p.splat);
        Ok(MethodDef {
            params: pnames,
            splat,
            kwparams,
            chunk: b.build(),
        })
    }

    fn compile_body_chunk(&mut self, body: &[Expr]) -> Result<Chunk, String> {
        // A method/proc body is its own chunk; a native-loop context from the
        // enclosing chunk does not cross into it (break/next become signals).
        let saved = std::mem::take(&mut self.loops);
        let mut b = ChunkBuilder::new();
        self.compile_seq(&mut b, body)?;
        self.loops = saved;
        Ok(b.build())
    }

    fn compile_expr(&mut self, b: &mut ChunkBuilder, e: &Expr) -> Result<(), String> {
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
                b.emit(Op::CallBuiltin(ops::MKSTR, parts.len() as u8), 0);
            }
            Expr::Symbol(s) => {
                self.kstr(b, s);
                b.emit(Op::CallBuiltin(ops::MKSYM, 1), 0);
            }
            Expr::Array(items) => {
                if items.iter().any(|it| matches!(it, Expr::Splat(_))) {
                    self.compile_spread(b, items)?;
                } else {
                    for it in items {
                        self.compile_expr(b, it)?;
                    }
                    b.emit(Op::CallBuiltin(ops::MKARRAY, argc(items.len())?), 0);
                }
            }
            Expr::Splat(e) => {
                // A bare splat outside a call/array just yields its array.
                self.compile_expr(b, e)?;
            }
            Expr::Hash(pairs) => {
                for (k, v) in pairs {
                    self.compile_expr(b, k)?;
                    self.compile_expr(b, v)?;
                }
                b.emit(Op::CallBuiltin(ops::MKHASH, argc(pairs.len() * 2)?), 0);
            }
            Expr::Range { lo, hi, exclusive } => {
                self.compile_expr(b, lo)?;
                self.compile_expr(b, hi)?;
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
            Expr::For { var, iter, body } => self.compile_for(b, var, iter, body)?,
            Expr::Case {
                subject,
                whens,
                els,
            } => self.compile_case(b, subject, whens, els)?,
            Expr::Call {
                recv,
                name,
                args,
                block,
            } => self.compile_call(b, recv, name, args, block)?,
            Expr::Index(recv, idx) => {
                self.compile_expr(b, recv)?;
                for i in idx {
                    self.compile_expr(b, i)?;
                }
                b.emit(Op::CallBuiltin(ops::INDEX_GET, argc(1 + idx.len())?), 0);
            }
            Expr::Def {
                name, params, body, ..
            } => {
                // A top-level `def` (singleton or not) registers a top-level
                // method; class-body `def`s are handled in `compile_class`.
                let def = self.compile_method(params, body)?;
                self.methods.push((name.clone(), def));
                // `def` evaluates to the method name as a symbol.
                self.kstr(b, name);
                b.emit(Op::CallBuiltin(ops::MKSYM, 1), 0);
            }
            Expr::Class {
                name,
                superclass,
                body,
            } => self.compile_class(b, name, superclass, body)?,
            Expr::Module { name, body } => self.compile_class(b, name, &None, body)?,
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
            Expr::Yield(args) => {
                for a in args {
                    self.compile_expr(b, a)?;
                }
                b.emit(Op::CallBuiltin(ops::YIELD, argc(args.len())?), 0);
            }
            Expr::Super(args) => match args {
                Some(args) => {
                    for a in args {
                        self.compile_expr(b, a)?;
                    }
                    b.emit(Op::CallBuiltin(ops::SUPER, argc(args.len())?), 0);
                }
                None => {
                    b.emit(Op::CallBuiltin(ops::SUPER_FWD, 0), 0);
                }
            },
        }
        Ok(())
    }

    fn compile_var_read(&mut self, b: &mut ChunkBuilder, kind: VarKind, name: &str) {
        let op = match kind {
            VarKind::Local => ops::GETLOCAL,
            VarKind::Instance => ops::GETIVAR,
            VarKind::Global => ops::GETGVAR,
            VarKind::Const => ops::GETCONST,
        };
        self.kstr(b, name);
        b.emit(Op::CallBuiltin(op, 1), 0);
    }

    fn compile_assign(
        &mut self,
        b: &mut ChunkBuilder,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), String> {
        match target {
            Expr::Var(kind, name) => {
                let op = match kind {
                    VarKind::Local => ops::SETLOCAL,
                    VarKind::Instance => ops::SETIVAR,
                    VarKind::Global => ops::SETGVAR,
                    VarKind::Const => ops::SETCONST,
                };
                self.kstr(b, name);
                self.compile_expr(b, value)?;
                b.emit(Op::CallBuiltin(op, 2), 0);
            }
            Expr::Index(recv, idx) => {
                self.compile_expr(b, recv)?;
                for i in idx {
                    self.compile_expr(b, i)?;
                }
                self.compile_expr(b, value)?;
                b.emit(Op::CallBuiltin(ops::INDEX_SET, argc(2 + idx.len())?), 0);
            }
            // Attribute assignment: `recv.attr = v` is the setter call
            // `recv.attr=(v)`.
            Expr::Call {
                recv: Some(r),
                name,
                args,
                block: None,
            } if args.is_empty() => {
                self.compile_expr(b, r)?;
                self.kstr(b, &format!("{name}="));
                self.compile_expr(b, value)?;
                b.emit(Op::CallBuiltin(ops::CALL_METHOD, 3), 0);
            }
            _ => return Err("invalid assignment target".into()),
        }
        Ok(())
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
        // rhs = a single Array (splat) or the coerced value list.
        let rhs = if values.len() == 1 {
            Expr::Call {
                recv: None,
                name: "Array".into(),
                args: vec![values[0].clone()],
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
                b.emit(Op::CallBuiltin(ops::TRUTHY, 1), 0);
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
            b.emit(Op::CallBuiltin(ops::TRUTHY, 1), 0);
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
            BinOp::Pow | BinOp::Div | BinOp::Mod | BinOp::Shl | BinOp::Shr | BinOp::Cmp
        ) {
            let name = match op {
                BinOp::Pow => "**",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
                _ => "<=>",
            };
            self.compile_expr(b, l)?;
            self.kstr(b, name);
            self.compile_expr(b, r)?;
            b.emit(Op::CallBuiltin(ops::CALL_METHOD, 3), 0);
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
            BinOp::And | BinOp::Or => unreachable!(),
        };
        b.emit(native, 0);
        Ok(())
    }

    fn compile_cond(&mut self, b: &mut ChunkBuilder, cond: &Expr) -> Result<(), String> {
        self.compile_expr(b, cond)?;
        b.emit(Op::CallBuiltin(ops::TRUTHY, 1), 0);
        Ok(())
    }

    fn compile_if(
        &mut self,
        b: &mut ChunkBuilder,
        cond: &Expr,
        then: &[Expr],
        elifs: &[(Expr, Vec<Expr>)],
        els: &Option<Vec<Expr>>,
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
        body: &[Expr],
    ) -> Result<(), String> {
        let start = b.current_pos();
        self.loops.push(LoopCtx {
            start,
            breaks: vec![],
            nexts: vec![],
        });
        self.compile_cond(b, cond)?;
        let exit = b.emit(Op::JumpIfFalse(0), 0);
        self.compile_seq(b, body)?;
        b.emit(Op::Pop, 0); // discard the body value each iteration
        b.emit(Op::Jump(start), 0);
        let end = b.current_pos();
        b.patch_jump(exit, end);
        let ctx = self.loops.pop().unwrap();
        for j in ctx.breaks {
            b.patch_jump(j, end);
        }
        for j in ctx.nexts {
            b.patch_jump(j, ctx.start);
        }
        // `while` evaluates to nil.
        b.emit(Op::LoadUndef, 0);
        Ok(())
    }

    fn compile_for(
        &mut self,
        b: &mut ChunkBuilder,
        var: &str,
        iter: &Expr,
        body: &[Expr],
    ) -> Result<(), String> {
        // `for v in iter … end` ≡ `iter.each { |v| … }` (block shares the frame,
        // so `v` and any assignments leak, matching Ruby's `for`).
        let block = Block {
            params: vec![var.to_string()],
            body: body.to_vec(),
        };
        let call = Expr::Call {
            recv: Some(Box::new(iter.clone())),
            name: "each".into(),
            args: vec![],
            block: Some(block),
        };
        self.compile_expr(b, &call)
    }

    fn compile_case(
        &mut self,
        b: &mut ChunkBuilder,
        subject: &Expr,
        whens: &[(Vec<Expr>, Vec<Expr>)],
        els: &Option<Vec<Expr>>,
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
                // label === subject   (CALL_METHOD wants [recv, name, arg])
                self.compile_expr(b, label)?;
                self.kstr(b, "===");
                self.compile_var_read(b, VarKind::Local, tmp);
                b.emit(Op::CallBuiltin(ops::CALL_METHOD, 3), 0);
                b.emit(Op::CallBuiltin(ops::TRUTHY, 1), 0);
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
        let proc_id = match block {
            Some(bl) => Some(self.compile_proc(bl)?),
            None => None,
        };
        // A call with a splat argument builds its argument array at runtime and
        // uses the array-spreading call ops. (Block + splat together is not yet
        // supported — tracked in BUGS.md.)
        if args.iter().any(|a| matches!(a, Expr::Splat(_))) {
            match recv {
                Some(r) => {
                    self.compile_expr(b, r)?;
                    self.kstr(b, name);
                    self.compile_spread(b, args)?;
                    b.emit(Op::CallBuiltin(ops::CALL_METHOD_ARR, 3), 0);
                }
                None => {
                    self.kstr(b, name);
                    self.compile_spread(b, args)?;
                    b.emit(Op::CallBuiltin(ops::CALL_ARR, 2), 0);
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
                match proc_id {
                    Some(id) => {
                        b.emit(Op::LoadInt(id as i64), 0);
                        b.emit(Op::CallBuiltin(ops::MKPROC, 1), 0);
                        b.emit(
                            Op::CallBuiltin(ops::CALL_METHOD_BLK, argc(3 + args.len())?),
                            0,
                        );
                    }
                    None => {
                        b.emit(Op::CallBuiltin(ops::CALL_METHOD, argc(2 + args.len())?), 0);
                    }
                }
            }
            None => {
                self.kstr(b, name);
                for a in args {
                    self.compile_expr(b, a)?;
                }
                match proc_id {
                    Some(id) => {
                        b.emit(Op::LoadInt(id as i64), 0);
                        b.emit(Op::CallBuiltin(ops::MKPROC, 1), 0);
                        b.emit(Op::CallBuiltin(ops::CALL_BLK, argc(2 + args.len())?), 0);
                    }
                    None => {
                        b.emit(Op::CallBuiltin(ops::CALL, argc(1 + args.len())?), 0);
                    }
                }
            }
        }
        Ok(())
    }

    fn compile_proc(&mut self, block: &Block) -> Result<usize, String> {
        self.compile_proc_body(&block.body, &block.params)
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
    fn compile_proc_body(&mut self, body: &[Expr], params: &[String]) -> Result<usize, String> {
        let chunk = self.compile_body_chunk(body)?;
        let id = self.procs.len();
        self.procs.push(ProcDef {
            params: params.to_vec(),
            chunk,
        });
        Ok(id)
    }

    /// Lower a `class`/`module` body: `def`s become instance methods (or class
    /// methods for `def self.m`), `attr_*` generate accessor methods, `include M`
    /// records a mixin. Other class-body statements are not yet executed
    /// (constants — tracked in BUGS.md).
    fn compile_class(
        &mut self,
        b: &mut ChunkBuilder,
        name: &str,
        superclass: &Option<String>,
        body: &[Expr],
    ) -> Result<(), String> {
        let mut methods: indexmap::IndexMap<String, MethodDef> = indexmap::IndexMap::new();
        let mut class_methods: indexmap::IndexMap<String, MethodDef> = indexmap::IndexMap::new();
        let mut includes: Vec<String> = Vec::new();
        for stmt in body {
            match stmt {
                Expr::Def {
                    name,
                    params,
                    body,
                    singleton,
                } => {
                    let def = self.compile_method(params, body)?;
                    if *singleton {
                        class_methods.insert(name.clone(), def);
                    } else {
                        methods.insert(name.clone(), def);
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
                // `include ModuleName` — record the mixin.
                Expr::Call {
                    recv: None,
                    name: m,
                    args,
                    ..
                } if m == "include" => {
                    for a in args {
                        if let Expr::Var(VarKind::Const, module) = a {
                            includes.push(module.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        self.classes.push((
            name.to_string(),
            ClassDef {
                superclass: superclass.clone(),
                methods,
                includes,
                class_methods,
            },
        ));
        // A class/module definition evaluates to nil here.
        b.emit(Op::LoadUndef, 0);
        Ok(())
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
            chunk: b.build(),
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
            chunk: b.build(),
        }
    }

    fn compile_begin(
        &mut self,
        b: &mut ChunkBuilder,
        body: &[Expr],
        rescues: &[Rescue],
        ensure: &Option<Vec<Expr>>,
    ) -> Result<(), String> {
        let body_id = self.compile_proc_body(body, &[])?;
        let mut rdefs = Vec::new();
        for r in rescues {
            let params: Vec<String> = r.binding.iter().cloned().collect();
            let rid = self.compile_proc_body(&r.body, &params)?;
            rdefs.push(RescueDef {
                classes: r.classes.clone(),
                binding: r.binding.clone(),
                body: rid,
            });
        }
        let ensure_id = match ensure {
            Some(e) => Some(self.compile_proc_body(e, &[])?),
            None => None,
        };
        let begin_id = self.begins.len();
        self.begins.push(BeginDef {
            body: body_id,
            rescues: rdefs,
            ensure: ensure_id,
        });
        b.emit(Op::LoadInt(begin_id as i64), 0);
        b.emit(Op::CallBuiltin(ops::BEGIN, 1), 0);
        Ok(())
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
            b.emit(Op::Pop, 0); // loop break/next carry no value in native form
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
}

enum FlowKind {
    Return,
    Break,
    Next,
}

/// The field name from an `attr_*` argument (`:sym` or `"str"`).
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

/// argc must fit in a `u8`; a call/collection wider than 255 is rejected.
fn argc(n: usize) -> Result<u8, String> {
    u8::try_from(n).map_err(|_| format!("too many arguments/elements ({n} > 255)"))
}
