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
use crate::host::{ops, MethodDef, ProcDef};
use fusevm::{Chunk, ChunkBuilder, Op, Value};
use std::sync::Arc;

/// The full output of compiling a program.
pub struct Program {
    pub main: Chunk,
    pub methods: Vec<(String, MethodDef)>,
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
                for it in items {
                    self.compile_expr(b, it)?;
                }
                b.emit(Op::CallBuiltin(ops::MKARRAY, argc(items.len())?), 0);
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
            Expr::Def { name, params, body } => {
                let chunk = self.compile_body_chunk(body)?;
                let pnames = params.iter().map(|p| p.name.clone()).collect();
                self.methods.push((
                    name.clone(),
                    MethodDef {
                        params: pnames,
                        chunk,
                    },
                ));
                // `def` evaluates to the method name as a symbol.
                self.kstr(b, name);
                b.emit(Op::CallBuiltin(ops::MKSYM, 1), 0);
            }
            Expr::Return(e) => self.compile_flow(b, e, ops::SIG_RETURN, FlowKind::Return)?,
            Expr::Break(e) => self.compile_flow(b, e, ops::SIG_BREAK, FlowKind::Break)?,
            Expr::Next(e) => self.compile_flow(b, e, ops::SIG_NEXT, FlowKind::Next)?,
            Expr::Yield(args) => {
                for a in args {
                    self.compile_expr(b, a)?;
                }
                b.emit(Op::CallBuiltin(ops::YIELD, argc(args.len())?), 0);
            }
        }
        Ok(())
    }

    fn compile_var_read(&mut self, b: &mut ChunkBuilder, kind: VarKind, name: &str) {
        if kind == VarKind::Local && name == "self" {
            b.emit(Op::LoadUndef, 0);
            return;
        }
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
            _ => return Err("invalid assignment target".into()),
        }
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
        if matches!(op, BinOp::Pow | BinOp::Div | BinOp::Mod) {
            let name = match op {
                BinOp::Pow => "**",
                BinOp::Div => "/",
                _ => "%",
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
        let chunk = self.compile_body_chunk(&block.body)?;
        let id = self.procs.len();
        self.procs.push(ProcDef {
            params: block.params.clone(),
            chunk,
        });
        Ok(id)
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

/// argc must fit in a `u8`; a call/collection wider than 255 is rejected.
fn argc(n: usize) -> Result<u8, String> {
    u8::try_from(n).map_err(|_| format!("too many arguments/elements ({n} > 255)"))
}
