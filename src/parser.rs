//! Recursive-descent / precedence-climbing parser for the Ruby subset rubylang
//! lowers. Produces `Vec<Stmt>`. Binary operators use a binding-power table
//! (`bp`); `**` and assignment are right-associative. Command (paren-less) calls
//! are recognized when a bare identifier is immediately followed by a token that
//! unambiguously starts an argument.
//!
//! Interpolation: double-quoted string bodies captured by the lexer are
//! re-scanned here for `#{ … }`, and each embedded expression is parsed with a
//! nested `Parser` over its own token stream.

use crate::ast::*;
use crate::lexer::{lex, Tok, Token};

/// A parsed block/lambda parameter list: the flat parameter names, the splat
/// index (if any), and the destructuring "prelude" assignments to prepend to
/// the block body (unpacking each `(a, b)` group from its temp parameter).
type BlockParams = (Vec<String>, Option<usize>, Vec<Expr>);

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// Counter for unique temporaries (safe-navigation desugaring).
    tmp: usize,
}

/// Parse a full program.
pub fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let toks = lex(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        tmp: 0,
    };
    p.program()
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].kind
    }
    fn line(&self) -> u32 {
        self.toks[self.pos].line
    }
    /// Whether whitespace precedes the current token.
    fn cur_space(&self) -> bool {
        self.toks[self.pos].space
    }
    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].kind.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn is_op(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if o == s)
    }
    fn is_kw(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Keyword(k) if k == s)
    }
    fn eat_op(&mut self, s: &str) -> bool {
        if self.is_op(s) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, s: &str) -> bool {
        if self.is_kw(s) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn expect_op(&mut self, s: &str) -> Result<(), String> {
        if self.eat_op(s) {
            Ok(())
        } else {
            Err(format!(
                "line {}: expected '{}', found '{}'",
                self.line(),
                s,
                self.peek()
            ))
        }
    }
    fn expect_kw(&mut self, s: &str) -> Result<(), String> {
        if self.eat_kw(s) {
            Ok(())
        } else {
            Err(format!(
                "line {}: expected '{}', found '{}'",
                self.line(),
                s,
                self.peek()
            ))
        }
    }
    /// Skip statement terminators (newlines / `;`).
    fn skip_terms(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semicolon) {
            self.advance();
        }
    }

    fn program(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();
        self.skip_terms();
        while !matches!(self.peek(), Tok::Eof) {
            let line = self.line();
            let e = self.statement()?;
            stmts.push(Stmt { expr: e, line });
            if !matches!(self.peek(), Tok::Eof) {
                if !matches!(self.peek(), Tok::Newline | Tok::Semicolon) {
                    return Err(format!(
                        "line {}: unexpected '{}'",
                        self.line(),
                        self.peek()
                    ));
                }
                self.skip_terms();
            }
        }
        Ok(stmts)
    }

    /// A body: statements until one of the given block-terminating keywords.
    fn body_until(&mut self, terms: &[&str]) -> Result<Vec<Expr>, String> {
        let mut out = Vec::new();
        self.skip_terms();
        while !matches!(self.peek(), Tok::Eof) {
            if let Tok::Keyword(k) = self.peek() {
                if terms.contains(&k.as_str()) {
                    break;
                }
            }
            out.push(self.statement()?);
            self.skip_terms();
        }
        Ok(out)
    }

    /// A statement is an expression optionally followed by a trailing modifier
    /// (`expr if cond`, `expr while cond`, …).
    fn statement(&mut self) -> Result<Expr, String> {
        let mut e = self.expr()?;
        // Parallel assignment is a statement-level form: `a, b = 1, 2`. A comma
        // after a bare lvalue (not consumed by a command call) starts the target
        // list. (Detecting this in `assign()` would misfire on array elements.)
        if self.is_op(",") {
            let mut targets = vec![e];
            while self.eat_op(",") {
                if self.eat_op("*") {
                    targets.push(Expr::Splat(Box::new(self.ternary()?)));
                } else {
                    targets.push(self.ternary()?);
                }
            }
            self.expect_op("=")?;
            let mut values = vec![self.ternary()?];
            while self.eat_op(",") {
                values.push(self.ternary()?);
            }
            e = Expr::MultiAssign { targets, values };
        }
        loop {
            if self.eat_kw("if") {
                let cond = self.expr()?;
                e = Expr::If {
                    cond: Box::new(cond),
                    then: vec![e],
                    elifs: vec![],
                    els: None,
                };
            } else if self.eat_kw("unless") {
                let cond = self.expr()?;
                e = Expr::If {
                    cond: Box::new(Expr::Unary(UnOp::Not, Box::new(cond))),
                    then: vec![e],
                    elifs: vec![],
                    els: None,
                };
            } else if self.eat_kw("while") {
                let cond = self.expr()?;
                // A `while` modifier attached to a `begin … end` is a post-test
                // loop: the body runs once before the condition is first checked.
                e = match do_while_body(e) {
                    Ok(body) => Expr::DoWhile {
                        cond: Box::new(cond),
                        body,
                    },
                    Err(e) => Expr::While {
                        cond: Box::new(cond),
                        body: vec![e],
                    },
                };
            } else if self.eat_kw("until") {
                let cond = self.expr()?;
                e = match do_while_body(e) {
                    Ok(body) => Expr::DoWhile {
                        cond: Box::new(Expr::Unary(UnOp::Not, Box::new(cond))),
                        body,
                    },
                    Err(e) => Expr::While {
                        cond: Box::new(Expr::Unary(UnOp::Not, Box::new(cond))),
                        body: vec![e],
                    },
                };
            } else if self.eat_kw("rescue") {
                // `expr rescue fallback` — a bare rescue catching StandardError.
                let handler = self.expr()?;
                e = Expr::Begin {
                    body: vec![e],
                    rescues: vec![Rescue {
                        classes: vec![],
                        binding: None,
                        body: vec![handler],
                    }],
                    ensure: None,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.assign()
    }

    fn assign(&mut self) -> Result<Expr, String> {
        let lhs = self.low_kw()?;
        // op-assignment and plain assignment
        if let Tok::Op(o) = self.peek() {
            let o = o.clone();
            let compound = matches!(
                o.as_str(),
                "+=" | "-="
                    | "*="
                    | "/="
                    | "%="
                    | "**="
                    | "|="
                    | "&="
                    | "^="
                    | "<<="
                    | ">>="
                    | "&&="
                    | "||="
            );
            if o == "=" {
                self.advance();
                let rhs = self.assign()?;
                return Ok(Expr::Assign(Box::new(lhs), Box::new(rhs)));
            } else if compound {
                self.advance();
                let rhs = self.assign()?;
                let op = match o.as_str() {
                    "+=" => BinOp::Add,
                    "-=" => BinOp::Sub,
                    "*=" => BinOp::Mul,
                    "/=" => BinOp::Div,
                    "%=" => BinOp::Mod,
                    "**=" => BinOp::Pow,
                    "|=" => BinOp::BitOr,
                    "&=" => BinOp::BitAnd,
                    "^=" => BinOp::BitXor,
                    "<<=" => BinOp::Shl,
                    ">>=" => BinOp::Shr,
                    "&&=" => BinOp::And,
                    "||=" => BinOp::Or,
                    _ => unreachable!(),
                };
                let combined = Expr::Binary(op, Box::new(lhs.clone()), Box::new(rhs));
                return Ok(Expr::Assign(Box::new(lhs), Box::new(combined)));
            }
        }
        Ok(lhs)
    }

    /// Low-precedence keyword operators `and` / `or` / `not`.
    fn low_kw(&mut self) -> Result<Expr, String> {
        if self.eat_kw("not") {
            let e = self.low_kw()?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(e)));
        }
        let mut lhs = self.ternary()?;
        loop {
            if self.eat_kw("and") {
                let rhs = self.ternary()?;
                lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
            } else if self.eat_kw("or") {
                let rhs = self.ternary()?;
                lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn ternary(&mut self) -> Result<Expr, String> {
        let cond = self.range()?;
        if self.eat_op("?") {
            let then = self.ternary()?;
            self.expect_op(":")?;
            let els = self.ternary()?;
            return Ok(Expr::If {
                cond: Box::new(cond),
                then: vec![then],
                elifs: vec![],
                els: Some(vec![els]),
            });
        }
        Ok(cond)
    }

    fn range(&mut self) -> Result<Expr, String> {
        // Beginless range: `..hi` / `...hi` (no low bound).
        if self.is_op("..") || self.is_op("...") {
            let exclusive = self.is_op("...");
            self.advance();
            let hi = self.binary(0)?;
            return Ok(Expr::Range {
                lo: None,
                hi: Some(Box::new(hi)),
                exclusive,
            });
        }
        let lo = self.binary(0)?;
        if self.is_op("..") || self.is_op("...") {
            let exclusive = self.is_op("...");
            self.advance();
            // Endless range: nothing that could start a bound follows the `..`.
            let hi = if self.range_end_follows() {
                None
            } else {
                Some(Box::new(self.binary(0)?))
            };
            return Ok(Expr::Range {
                lo: Some(Box::new(lo)),
                hi,
                exclusive,
            });
        }
        Ok(lo)
    }

    /// Whether the current token ends an endless range (`lo..` with no high
    /// bound) — a closing bracket, separator, terminator, or a block keyword.
    fn range_end_follows(&self) -> bool {
        matches!(self.peek(), Tok::Newline | Tok::Semicolon | Tok::Eof)
            || matches!(self.peek(), Tok::Op(o)
                if matches!(o.as_str(), "]" | ")" | "}" | ","))
            || matches!(self.peek(), Tok::Keyword(k)
                if matches!(k.as_str(), "then" | "do" | "end"))
    }

    /// Binding power of a binary operator token, or None if not a binary op.
    fn bp(op: &str) -> Option<(u8, BinOp)> {
        Some(match op {
            "||" => (1, BinOp::Or),
            "&&" => (2, BinOp::And),
            "==" | "!=" | "<=>" | "===" | "=~" | "!~" => (3, matchop(op)),
            "<" | "<=" | ">" | ">=" => (4, matchop(op)),
            "|" | "^" => (5, matchop(op)),
            "&" => (6, BinOp::BitAnd),
            "<<" | ">>" => (7, matchop(op)),
            "+" | "-" => (8, matchop(op)),
            "*" | "/" | "%" => (9, matchop(op)),
            _ => return None,
        })
    }

    fn binary(&mut self, min_bp: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        while let Tok::Op(o) = self.peek() {
            let o = o.clone();
            let Some((bp, op)) = Self::bp(&o) else { break };
            if bp < min_bp {
                break;
            }
            self.advance();
            // left-assoc: next level binds tighter
            let rhs = self.binary(bp + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat_op("-") {
            return Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary()?)));
        }
        if self.eat_op("!") {
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.unary()?)));
        }
        if self.eat_op("~") {
            return Ok(Expr::Unary(UnOp::BitNot, Box::new(self.unary()?)));
        }
        if self.eat_op("+") {
            return self.unary();
        }
        self.pow()
    }

    fn pow(&mut self) -> Result<Expr, String> {
        let base = self.postfix()?;
        if self.eat_op("**") {
            // right-associative
            let exp = self.unary()?;
            return Ok(Expr::Binary(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    /// Primary followed by any chain of `.method(args){block}`, `[index]`.
    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            let safe = self.is_op("&.");
            if self.eat_op(".") || self.eat_op("&.") {
                // `recv.(args)` is sugar for `recv.call(args)` (Proc/Method).
                let name = if self.is_op("(") {
                    "call".to_string()
                } else {
                    self.method_name()?
                };
                let (args, block) = self.call_tail()?;
                if safe {
                    // `a&.b(args)` desugars to `(t = a).nil? ? nil : t.b(args)`,
                    // evaluating the receiver once and skipping the call on nil.
                    self.tmp += 1;
                    let tmp = format!("__safenav{}__", self.tmp);
                    let assign = Expr::Assign(
                        Box::new(Expr::Var(VarKind::Local, tmp.clone())),
                        Box::new(e),
                    );
                    e = Expr::If {
                        cond: Box::new(Expr::Call {
                            recv: Some(Box::new(assign)),
                            name: "nil?".to_string(),
                            args: vec![],
                            block: None,
                        }),
                        then: vec![Expr::Nil],
                        elifs: vec![],
                        els: Some(vec![Expr::Call {
                            recv: Some(Box::new(Expr::Var(VarKind::Local, tmp))),
                            name,
                            args,
                            block,
                        }]),
                    };
                } else {
                    e = Expr::Call {
                        recv: Some(Box::new(e)),
                        name,
                        args,
                        block,
                    };
                }
            } else if self.is_op("[") {
                self.advance();
                let mut idx = Vec::new();
                if !self.is_op("]") {
                    idx.push(self.expr()?);
                    while self.eat_op(",") {
                        idx.push(self.expr()?);
                    }
                }
                self.expect_op("]")?;
                e = Expr::Index(Box::new(e), idx);
            } else if self.eat_op("::") {
                // Foo::Bar constant access → treat as const ref via Call for now
                let name = self.method_name()?;
                e = Expr::Call {
                    recv: Some(Box::new(e)),
                    name,
                    args: vec![],
                    block: None,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn method_name(&mut self) -> Result<String, String> {
        match self.advance() {
            Tok::Ident(s) | Tok::Const(s) | Tok::Keyword(s) => Ok(s),
            // Operator method names: `def <=>`, `def +`, `def ==`, `def [](i)`,
            // `def []=(i, v)`, etc.
            Tok::Op(o) if o == "[" => {
                self.expect_op("]")?;
                if self.eat_op("=") {
                    Ok("[]=".to_string())
                } else {
                    Ok("[]".to_string())
                }
            }
            Tok::Op(o) if is_operator_method(&o) => Ok(o),
            other => Err(format!(
                "line {}: expected method name, found '{}'",
                self.line(),
                other
            )),
        }
    }

    /// Parse the argument list + optional block that follow a method name.
    /// Trailing `key: value` keyword arguments are collected into a single Hash
    /// argument (symbol keys), matching Ruby's implicit keyword hash.
    fn call_tail(&mut self) -> Result<(Vec<Expr>, Option<Block>), String> {
        let mut args = Vec::new();
        let mut amp_block = None;
        let mut kwargs: Vec<(Expr, Expr)> = Vec::new();
        let mut kwsplats: Vec<Expr> = Vec::new();
        if self.eat_op("(") {
            if !self.is_op(")") {
                self.arg_or_amp(&mut args, &mut amp_block, &mut kwargs, &mut kwsplats)?;
                while self.eat_op(",") {
                    self.arg_or_amp(&mut args, &mut amp_block, &mut kwargs, &mut kwsplats)?;
                }
            }
            self.expect_op(")")?;
        }
        Self::push_trailing_kwargs(&mut args, kwargs, kwsplats);
        let block = self.maybe_block()?.or(amp_block);
        Ok((args, block))
    }

    /// Append the trailing keyword hash to `args`: the literal `key: value` pairs,
    /// then each `**hash` merged in (last-wins), matching Ruby's keyword-splat
    /// semantics.
    fn push_trailing_kwargs(args: &mut Vec<Expr>, kwargs: Vec<(Expr, Expr)>, kwsplats: Vec<Expr>) {
        if kwargs.is_empty() && kwsplats.is_empty() {
            return;
        }
        let mut trailing = Expr::Hash(kwargs);
        for s in kwsplats {
            trailing = Expr::Call {
                recv: Some(Box::new(trailing)),
                name: "merge".into(),
                args: vec![s],
                block: None,
            };
        }
        args.push(trailing);
    }

    /// Parse one call argument, recognizing a `key: value` keyword argument, a
    /// `**hash` keyword splat, a `&:sym` block-pass, and a `*expr` splat.
    fn arg_or_amp(
        &mut self,
        args: &mut Vec<Expr>,
        amp_block: &mut Option<Block>,
        kwargs: &mut Vec<(Expr, Expr)>,
        kwsplats: &mut Vec<Expr>,
    ) -> Result<(), String> {
        // `**hash` — spread a hash as keyword arguments.
        if self.is_op("**") {
            self.advance();
            kwsplats.push(self.arg()?);
            return Ok(());
        }
        // `key: value` keyword argument (symbol key).
        if let Tok::Ident(key) = self.peek().clone() {
            if matches!(&self.toks[self.pos + 1].kind, Tok::Op(o) if o == ":") {
                self.advance(); // key
                self.advance(); // :
                let v = self.arg()?;
                kwargs.push((Expr::Symbol(key), v));
                return Ok(());
            }
        }
        // `*expr` — splat the array's elements into the argument list.
        if self.is_op("*") {
            self.advance();
            args.push(Expr::Splat(Box::new(self.arg()?)));
            return Ok(());
        }
        if self.is_op("&") {
            self.advance(); // &
                            // `&:sym` — inline the send as `{ |__blkx__| __blkx__.sym }`.
            if let Tok::Symbol(s) = self.peek().clone() {
                self.advance(); // :sym
                                // `{ |*__symargs__| :sym.to_proc.call(*__symargs__) }` —
                                // `&:sym` is `Symbol#to_proc`, which sends `sym` to its
                                // first argument and forwards the rest (`reduce(&:+)` →
                                // `acc.+(x)`). A lone splat parameter is used so a single
                                // *array* argument is NOT auto-splatted — `[[1,2]].map(&:sum)`
                                // must call `sum` on `[1, 2]`, not on `1` with arg `2`.
                let sym_to_proc = Expr::Call {
                    recv: Some(Box::new(Expr::Symbol(s))),
                    name: "to_proc".into(),
                    args: vec![],
                    block: None,
                };
                let call = Expr::Call {
                    recv: Some(Box::new(sym_to_proc)),
                    name: "call".into(),
                    args: vec![Expr::Splat(Box::new(Expr::Var(
                        VarKind::Local,
                        "__symargs__".into(),
                    )))],
                    block: None,
                };
                *amp_block = Some(Block {
                    params: vec!["__symargs__".into()],
                    splat: Some(0),
                    body: vec![call],
                });
                return Ok(());
            }
            // `&expr` — block-pass any callable (Proc / Method / to_proc-able):
            // `{ |__blkx__| (expr).call(__blkx__) }`.
            let e = self.arg()?;
            let call = Expr::Call {
                recv: Some(Box::new(e)),
                name: "call".into(),
                args: vec![Expr::Var(VarKind::Local, "__blkx__".into())],
                block: None,
            };
            *amp_block = Some(Block {
                params: vec!["__blkx__".into()],
                splat: None,
                body: vec![call],
            });
            return Ok(());
        }
        // `key => value` trailing pair (arbitrary key expr) — collected into the
        // implicit trailing hash alongside any `key: value` pairs.
        let e = self.arg()?;
        if self.is_op("=>") {
            self.advance();
            let v = self.arg()?;
            kwargs.push((e, v));
            return Ok(());
        }
        args.push(e);
        Ok(())
    }

    /// An argument allows `key: val` and `key => val` pair sugar (collapsed into
    /// a trailing hash by the caller is out of scope; here we just parse a value).
    fn arg(&mut self) -> Result<Expr, String> {
        self.expr()
    }

    fn maybe_block(&mut self) -> Result<Option<Block>, String> {
        if self.eat_kw("do") {
            let (params, splat, preludes) = self.block_params()?;
            let rest = self.body_until(&["end"])?;
            self.expect_kw("end")?;
            let body = self.prepend_preludes(preludes, rest);
            return Ok(Some(Block {
                params,
                splat,
                body,
            }));
        }
        if self.is_op("{") {
            self.advance();
            let (params, splat, preludes) = self.block_params()?;
            let mut body = Vec::new();
            self.skip_terms();
            while !self.is_op("}") && !matches!(self.peek(), Tok::Eof) {
                body.push(self.statement()?);
                self.skip_terms();
            }
            self.expect_op("}")?;
            let body = self.prepend_preludes(preludes, body);
            return Ok(Some(Block {
                params,
                splat,
                body,
            }));
        }
        Ok(None)
    }

    /// Prepend the destructuring `preludes` (parallel-assignment unpackings) to
    /// the block `body`, so `(a, b)` parameters are unpacked before the body runs.
    fn prepend_preludes(&self, mut preludes: Vec<Expr>, body: Vec<Expr>) -> Vec<Expr> {
        if preludes.is_empty() {
            return body;
        }
        preludes.extend(body);
        preludes
    }

    /// Parse a block/lambda parameter list, returning the flat parameter names,
    /// the splat index (if any), and a list of "prelude" assignments that
    /// destructure any `(a, b)` parameters into their names. A destructuring
    /// parameter is bound to a fresh temp name and unpacked at the top of the
    /// block body via ordinary parallel assignment (`a, b = __tmp`), so nested
    /// patterns become a sequence of flat assignments.
    fn block_params(&mut self) -> Result<BlockParams, String> {
        let mut params = Vec::new();
        let mut splat = None;
        let mut preludes = Vec::new();
        if self.eat_op("|") {
            if !self.is_op("|") {
                self.block_param(&mut params, &mut splat, &mut preludes)?;
                while self.eat_op(",") {
                    if self.is_op("|") {
                        break; // trailing comma
                    }
                    self.block_param(&mut params, &mut splat, &mut preludes)?;
                }
            }
            self.expect_op("|")?;
        }
        Ok((params, splat, preludes))
    }

    /// One block/lambda parameter: `name`, `*rest` (the splat collects surplus
    /// positional args), or a `(a, b)` destructuring group. Records the splat
    /// index when it sees `*`; a destructuring group pushes a fresh temp name as
    /// the parameter and appends its unpacking assignment(s) to `preludes`.
    fn block_param(
        &mut self,
        params: &mut Vec<String>,
        splat: &mut Option<usize>,
        preludes: &mut Vec<Expr>,
    ) -> Result<(), String> {
        if self.is_op("(") {
            let (temp, assigns) = self.destructure_param()?;
            params.push(temp);
            preludes.extend(assigns);
            return Ok(());
        }
        if self.eat_op("*") {
            *splat = Some(params.len());
        }
        params.push(self.ident_name()?);
        Ok(())
    }

    /// Parse a `(a, b, *rest, (c, d))` destructuring group. Returns a fresh temp
    /// name (the actual parameter, which receives the whole array argument) and
    /// the parallel-assignment statements that unpack it — this level first, then
    /// any nested groups, so temps are always filled before they are read.
    fn destructure_param(&mut self) -> Result<(String, Vec<Expr>), String> {
        self.expect_op("(")?;
        let temp = format!("__destructure_{}", self.tmp);
        self.tmp += 1;
        let mut targets: Vec<Expr> = Vec::new();
        let mut nested: Vec<Expr> = Vec::new();
        loop {
            if self.eat_op("*") {
                targets.push(Expr::Splat(Box::new(Expr::Var(
                    VarKind::Local,
                    self.ident_name()?,
                ))));
            } else if self.is_op("(") {
                let (inner_temp, inner_assigns) = self.destructure_param()?;
                targets.push(Expr::Var(VarKind::Local, inner_temp));
                nested.extend(inner_assigns);
            } else {
                targets.push(Expr::Var(VarKind::Local, self.ident_name()?));
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        let mut out = vec![Expr::MultiAssign {
            targets,
            values: vec![Expr::Var(VarKind::Local, temp.clone())],
        }];
        out.extend(nested);
        Ok((temp, out))
    }

    fn ident_name(&mut self) -> Result<String, String> {
        match self.advance() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!(
                "line {}: expected identifier, found '{}'",
                self.line(),
                other
            )),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Tok::Int(n) => {
                self.advance();
                Ok(Expr::Int(n))
            }
            Tok::Float(x) => {
                self.advance();
                Ok(Expr::Float(x))
            }
            Tok::Str(s, dq) => {
                self.advance();
                if dq {
                    Ok(Expr::Str(scan_interp(&s)?))
                } else {
                    Ok(Expr::Str(vec![StrPart::Lit(s)]))
                }
            }
            Tok::Regex(pat, flags) => {
                self.advance();
                Ok(Expr::Regex(pat, flags))
            }
            Tok::Symbol(s) => {
                self.advance();
                Ok(Expr::Symbol(s))
            }
            Tok::IVar(s) => {
                self.advance();
                Ok(Expr::Var(VarKind::Instance, s))
            }
            Tok::CVar(s) => {
                self.advance();
                Ok(Expr::Var(VarKind::Class, s))
            }
            Tok::GVar(s) => {
                self.advance();
                Ok(Expr::Var(VarKind::Global, s))
            }
            Tok::Const(s) => {
                self.advance();
                // Const(args) is a call (e.g. Integer(x)); otherwise a const ref.
                if self.is_op("(") {
                    let (args, block) = self.call_tail()?;
                    Ok(Expr::Call {
                        recv: None,
                        name: s,
                        args,
                        block,
                    })
                } else {
                    Ok(Expr::Var(VarKind::Const, s))
                }
            }
            Tok::Op(ref o) if o == "(" => {
                self.advance();
                self.skip_terms();
                let e = self.expr()?;
                self.skip_terms();
                self.expect_op(")")?;
                Ok(e)
            }
            Tok::Op(ref o) if o == "[" => self.array_lit(),
            Tok::Op(ref o) if o == "->" => self.lambda_lit(),
            Tok::Op(ref o) if o == "{" => self.hash_lit(),
            Tok::Op(ref o) if o == "::" => {
                self.advance();
                let s = match self.advance() {
                    Tok::Const(s) => s,
                    other => {
                        return Err(format!(
                            "line {}: expected constant, found '{}'",
                            self.line(),
                            other
                        ))
                    }
                };
                Ok(Expr::Var(VarKind::Const, s))
            }
            Tok::Keyword(k) => self.keyword_primary(&k),
            Tok::Ident(name) => {
                self.advance();
                self.ident_primary(name)
            }
            other => Err(format!("line {}: unexpected '{}'", self.line(), other)),
        }
    }

    /// A bare identifier: a local var read, a paren call, or a command
    /// (paren-less) call.
    fn ident_primary(&mut self, name: String) -> Result<Expr, String> {
        // `defined?(expr)` / `defined? expr` — describe the operand without
        // evaluating it. Parenthesized form takes any expression; the bare form
        // takes a single postfix operand (`defined? @x`, `defined? Foo.bar`).
        if name == "defined?" {
            let operand = if self.is_op("(") {
                self.advance();
                let e = self.expr()?;
                self.expect_op(")")?;
                e
            } else {
                self.postfix()?
            };
            return Ok(Expr::Defined(Box::new(operand)));
        }
        // `foo(x)` — no space before `(` — is a parenthesized call.
        if self.is_op("(") && !self.cur_space() {
            let (args, block) = self.call_tail()?;
            return Ok(Expr::Call {
                recv: None,
                name,
                args,
                block,
            });
        }
        // command call: a space, then a token that starts an argument
        // (`puts x`, `puts (a).b`, `greet name: "x"`).
        if self.cur_space() && self.starts_command_arg() {
            let mut args = Vec::new();
            let mut amp_block = None;
            let mut kwargs: Vec<(Expr, Expr)> = Vec::new();
            let mut kwsplats: Vec<Expr> = Vec::new();
            self.arg_or_amp(&mut args, &mut amp_block, &mut kwargs, &mut kwsplats)?;
            while self.eat_op(",") {
                self.arg_or_amp(&mut args, &mut amp_block, &mut kwargs, &mut kwsplats)?;
            }
            Self::push_trailing_kwargs(&mut args, kwargs, kwsplats);
            let block = self.maybe_block()?.or(amp_block);
            return Ok(Expr::Call {
                recv: None,
                name,
                args,
                block,
            });
        }
        // a trailing block with no args (`foo { ... }` / `foo do ... end`)
        if self.is_op("{") || self.is_kw("do") {
            let block = self.maybe_block()?;
            return Ok(Expr::Call {
                recv: None,
                name,
                args: vec![],
                block,
            });
        }
        Ok(Expr::Var(VarKind::Local, name))
    }

    /// Does the current token unambiguously start a command-call argument?
    /// Conservative: literals, strings, symbols, sigil vars, `[`, constants, and
    /// the keywords nil/true/false. Excludes operators (so `x - 1` stays binary).
    fn starts_command_arg(&self) -> bool {
        match self.peek() {
            Tok::Int(_)
            | Tok::Float(_)
            | Tok::Str(_, _)
            | Tok::Regex(_, _)
            | Tok::Symbol(_)
            | Tok::IVar(_)
            | Tok::CVar(_)
            | Tok::GVar(_)
            | Tok::Const(_)
            | Tok::Ident(_) => true,
            Tok::Keyword(k) => matches!(k.as_str(), "nil" | "true" | "false" | "self"),
            // A tight unary sign (`puts -7` — space before `-`, none after) is a
            // command argument; a spaced `x - 7` stays a binary operator (the
            // caller only reaches here when a space already precedes the token).
            Tok::Op(o) if o == "-" || o == "+" => {
                !self.toks.get(self.pos + 1).map(|t| t.space).unwrap_or(true)
            }
            // A tight `*args` / `**opts` / `&blk` (space before, none after) is a
            // splat/block-pass command argument, not a binary operator.
            Tok::Op(o) if o == "*" || o == "**" || o == "&" => {
                !self.toks.get(self.pos + 1).map(|t| t.space).unwrap_or(true)
            }
            Tok::Op(o) => o == "[" || o == "(",
            _ => false,
        }
    }

    fn keyword_primary(&mut self, k: &str) -> Result<Expr, String> {
        match k {
            "nil" => {
                self.advance();
                Ok(Expr::Nil)
            }
            "true" => {
                self.advance();
                Ok(Expr::True)
            }
            "false" => {
                self.advance();
                Ok(Expr::False)
            }
            "self" => {
                self.advance();
                Ok(Expr::SelfExpr)
            }
            "if" => self.if_expr(false),
            "unless" => self.if_expr(true),
            "while" => self.while_expr(false),
            "until" => self.while_expr(true),
            "for" => self.for_expr(),
            "case" => self.case_expr(),
            "def" => self.def_expr(),
            "class" => self.class_expr(),
            "module" => self.module_expr(),
            "alias" => self.alias_expr(),
            "return" => {
                self.advance();
                Ok(Expr::Return(self.opt_value()?))
            }
            "break" => {
                self.advance();
                Ok(Expr::Break(self.opt_value()?))
            }
            "next" => {
                self.advance();
                Ok(Expr::Next(self.opt_value()?))
            }
            "retry" => {
                self.advance();
                Ok(Expr::Retry)
            }
            "yield" => {
                self.advance();
                let (args, _) = if self.is_op("(") {
                    self.call_tail()?
                } else if self.starts_command_arg() {
                    let mut a = vec![self.arg()?];
                    while self.eat_op(",") {
                        a.push(self.arg()?);
                    }
                    (a, None)
                } else {
                    (vec![], None)
                };
                Ok(Expr::Yield(args))
            }
            "super" => {
                self.advance();
                // `super` (no parens) forwards the current args; `super(...)` /
                // `super a, b` passes explicit args.
                if self.is_op("(") {
                    let (args, _) = self.call_tail()?;
                    Ok(Expr::Super(Some(args)))
                } else if self.cur_space() && self.starts_command_arg() {
                    let mut args = vec![self.arg()?];
                    while self.eat_op(",") {
                        args.push(self.arg()?);
                    }
                    Ok(Expr::Super(Some(args)))
                } else {
                    Ok(Expr::Super(None))
                }
            }
            "begin" => {
                self.advance();
                let body = self.body_until(&["rescue", "ensure", "end"])?;
                let (rescues, ensure) = self.rescue_tail()?;
                self.expect_kw("end")?;
                Ok(Expr::Begin {
                    body,
                    rescues,
                    ensure,
                })
            }
            other => Err(format!(
                "line {}: unexpected keyword '{}'",
                self.line(),
                other
            )),
        }
    }

    fn opt_value(&mut self) -> Result<Option<Box<Expr>>, String> {
        if matches!(self.peek(), Tok::Newline | Tok::Semicolon | Tok::Eof)
            || self.is_kw("end")
            || self.is_kw("if")
            || self.is_kw("unless")
        {
            Ok(None)
        } else {
            Ok(Some(Box::new(self.expr()?)))
        }
    }

    fn if_expr(&mut self, negate: bool) -> Result<Expr, String> {
        self.advance(); // if/unless
        let mut cond = self.expr()?;
        if negate {
            cond = Expr::Unary(UnOp::Not, Box::new(cond));
        }
        self.eat_kw("then");
        self.eat_op(":");
        let then = self.body_until(&["elsif", "else", "end"])?;
        let mut elifs = Vec::new();
        while self.eat_kw("elsif") {
            let c = self.expr()?;
            self.eat_kw("then");
            let b = self.body_until(&["elsif", "else", "end"])?;
            elifs.push((c, b));
        }
        let els = if self.eat_kw("else") {
            Some(self.body_until(&["end"])?)
        } else {
            None
        };
        self.expect_kw("end")?;
        Ok(Expr::If {
            cond: Box::new(cond),
            then,
            elifs,
            els,
        })
    }

    fn while_expr(&mut self, negate: bool) -> Result<Expr, String> {
        self.advance();
        let mut cond = self.expr()?;
        if negate {
            cond = Expr::Unary(UnOp::Not, Box::new(cond));
        }
        self.eat_kw("do");
        let body = self.body_until(&["end"])?;
        self.expect_kw("end")?;
        Ok(Expr::While {
            cond: Box::new(cond),
            body,
        })
    }

    fn for_expr(&mut self) -> Result<Expr, String> {
        self.advance();
        let var = self.ident_name()?;
        self.expect_kw("in")?;
        let iter = self.expr()?;
        self.eat_kw("do");
        let body = self.body_until(&["end"])?;
        self.expect_kw("end")?;
        Ok(Expr::For {
            var,
            iter: Box::new(iter),
            body,
        })
    }

    fn case_expr(&mut self) -> Result<Expr, String> {
        self.advance();
        let subject = self.expr()?;
        self.skip_terms();
        // `case/in` (pattern matching) is chosen by the first clause keyword.
        if self.is_kw("in") {
            return self.case_in(subject);
        }
        let mut whens = Vec::new();
        while self.eat_kw("when") {
            let mut labels = vec![self.expr()?];
            while self.eat_op(",") {
                labels.push(self.expr()?);
            }
            self.eat_kw("then");
            let body = self.body_until(&["when", "else", "end"])?;
            whens.push((labels, body));
        }
        let els = if self.eat_kw("else") {
            Some(self.body_until(&["end"])?)
        } else {
            None
        };
        self.expect_kw("end")?;
        Ok(Expr::Case {
            subject: Box::new(subject),
            whens,
            els,
        })
    }

    /// `case subj; in pattern [if/unless guard]; body; … [else …] end`.
    fn case_in(&mut self, subject: Expr) -> Result<Expr, String> {
        let mut clauses = Vec::new();
        while self.eat_kw("in") {
            let pattern = self.parse_pattern()?;
            let guard = if self.eat_kw("if") {
                Some(self.expr()?)
            } else if self.eat_kw("unless") {
                Some(Expr::Unary(UnOp::Not, Box::new(self.expr()?)))
            } else {
                None
            };
            self.eat_kw("then");
            let body = self.body_until(&["in", "else", "end"])?;
            clauses.push(InClause {
                pattern,
                guard,
                body,
            });
        }
        let els = if self.eat_kw("else") {
            Some(self.body_until(&["end"])?)
        } else {
            None
        };
        self.expect_kw("end")?;
        Ok(Expr::CaseIn {
            subject: Box::new(subject),
            clauses,
            els,
        })
    }

    /// `alias new_name old_name` — desugars to `alias_method(:new, :old)`.
    fn alias_expr(&mut self) -> Result<Expr, String> {
        self.advance(); // alias
        let new_name = self.alias_name()?;
        let old_name = self.alias_name()?;
        Ok(Expr::Call {
            recv: None,
            name: "alias_method".to_string(),
            args: vec![Expr::Symbol(new_name), Expr::Symbol(old_name)],
            block: None,
        })
    }

    /// A method name in an `alias` clause: a bareword, a `:symbol`, or an operator.
    fn alias_name(&mut self) -> Result<String, String> {
        match self.advance() {
            Tok::Ident(s) | Tok::Const(s) | Tok::Symbol(s) => Ok(s),
            Tok::Op(o) => Ok(o),
            other => Err(format!("line {}: bad alias name '{other}'", self.line())),
        }
    }

    /// A full pattern: alternatives joined by `|`, with an optional `=> name`
    /// binding of the whole match.
    fn parse_pattern(&mut self) -> Result<Pattern, String> {
        let mut alts = vec![self.parse_pattern_binding()?];
        while self.eat_op("|") {
            alts.push(self.parse_pattern_binding()?);
        }
        if alts.len() == 1 {
            Ok(alts.pop().unwrap())
        } else {
            Ok(Pattern::Or(alts))
        }
    }

    fn parse_pattern_binding(&mut self) -> Result<Pattern, String> {
        let p = self.parse_pattern_primary()?;
        if self.eat_op("=>") {
            Ok(Pattern::As(Box::new(p), self.ident_name()?))
        } else {
            Ok(p)
        }
    }

    fn parse_pattern_primary(&mut self) -> Result<Pattern, String> {
        match self.peek().clone() {
            Tok::Op(o) if o == "[" => {
                self.advance();
                let elems = self.parse_array_pattern("]")?;
                self.expect_op("]")?;
                Ok(Pattern::Array(elems))
            }
            Tok::Op(o) if o == "{" => {
                self.advance();
                let (pairs, rest) = self.parse_hash_pattern()?;
                self.expect_op("}")?;
                Ok(Pattern::Hash(pairs, rest))
            }
            Tok::Op(o) if o == "^" => {
                self.advance();
                Ok(Pattern::Pin(self.primary()?))
            }
            Tok::Op(o) if o == "*" => {
                self.advance();
                let name = match self.peek().clone() {
                    Tok::Ident(n) => {
                        self.advance();
                        Some(n)
                    }
                    _ => None,
                };
                Ok(Pattern::Splat(name))
            }
            Tok::Const(name) => {
                self.advance();
                // `Const[...]` / `Const(...)` deconstruction.
                if self.eat_op("[") {
                    let elems = self.parse_array_pattern("]")?;
                    self.expect_op("]")?;
                    Ok(Pattern::Const(name, Some(Box::new(Pattern::Array(elems)))))
                } else if self.eat_op("(") {
                    let elems = self.parse_array_pattern(")")?;
                    self.expect_op(")")?;
                    Ok(Pattern::Const(name, Some(Box::new(Pattern::Array(elems)))))
                } else {
                    Ok(Pattern::Const(name, None))
                }
            }
            Tok::Ident(n) => {
                self.advance();
                Ok(Pattern::Bind(n))
            }
            // Everything else is a literal / range value matched with `===`.
            _ => Ok(Pattern::Value(self.range()?)),
        }
    }

    /// Comma-separated patterns until `close`.
    fn parse_array_pattern(&mut self, close: &str) -> Result<Vec<Pattern>, String> {
        let mut elems = Vec::new();
        self.skip_terms();
        if !self.is_op(close) {
            elems.push(self.parse_pattern()?);
            while self.eat_op(",") {
                self.skip_terms();
                if self.is_op(close) {
                    break;
                }
                elems.push(self.parse_pattern()?);
            }
        }
        self.skip_terms();
        Ok(elems)
    }

    /// `key:`, `key: subpattern`, and a trailing `**rest`/`**nil`, until `}`.
    #[allow(clippy::type_complexity)]
    fn parse_hash_pattern(&mut self) -> Result<(Vec<(String, Option<Pattern>)>, bool), String> {
        let mut pairs = Vec::new();
        let mut rest = false;
        self.skip_terms();
        while !self.is_op("}") {
            if self.eat_op("**") {
                // `**rest` / `**nil` — both just mean "allow other keys".
                if matches!(self.peek(), Tok::Ident(_)) || self.is_kw("nil") {
                    self.advance();
                }
                rest = true;
            } else {
                let key = match self.advance() {
                    Tok::Ident(k) | Tok::Const(k) => k,
                    Tok::Symbol(k) => k,
                    other => {
                        return Err(format!(
                            "line {}: bad hash pattern key '{other}'",
                            self.line()
                        ))
                    }
                };
                self.expect_op(":")?;
                // `key:` shorthand binds `key`; otherwise a subpattern follows.
                let sub = if self.is_op(",") || self.is_op("}") {
                    None
                } else {
                    Some(self.parse_pattern()?)
                };
                pairs.push((key, sub));
            }
            self.skip_terms();
            if !self.eat_op(",") {
                break;
            }
            self.skip_terms();
        }
        Ok((pairs, rest))
    }

    fn class_expr(&mut self) -> Result<Expr, String> {
        self.advance(); // class
        let name = match self.advance() {
            Tok::Const(s) => s,
            other => {
                return Err(format!(
                    "line {}: expected class name, found '{}'",
                    self.line(),
                    other
                ))
            }
        };
        let superclass = if self.eat_op("<") {
            match self.advance() {
                Tok::Const(s) => Some(s),
                other => {
                    return Err(format!(
                        "line {}: expected superclass, found '{}'",
                        self.line(),
                        other
                    ))
                }
            }
        } else {
            None
        };
        let body = self.body_until(&["end"])?;
        self.expect_kw("end")?;
        Ok(Expr::Class {
            name,
            superclass,
            body,
        })
    }

    fn module_expr(&mut self) -> Result<Expr, String> {
        self.advance(); // module
        let name = match self.advance() {
            Tok::Const(s) => s,
            other => {
                return Err(format!(
                    "line {}: expected module name, found '{}'",
                    self.line(),
                    other
                ))
            }
        };
        let body = self.body_until(&["end"])?;
        self.expect_kw("end")?;
        Ok(Expr::Module { name, body })
    }

    /// Parse the `rescue …` clauses and optional `ensure …` after a body.
    fn rescue_tail(&mut self) -> Result<(Vec<Rescue>, Option<Vec<Expr>>), String> {
        let mut rescues = Vec::new();
        while self.eat_kw("rescue") {
            let mut classes = Vec::new();
            // optional list of exception class names
            while let Tok::Const(c) = self.peek().clone() {
                self.advance();
                classes.push(c);
                if !self.eat_op(",") {
                    break;
                }
            }
            let binding = if self.eat_op("=>") {
                Some(self.ident_name()?)
            } else {
                None
            };
            self.eat_kw("then");
            let body = self.body_until(&["rescue", "ensure", "end"])?;
            rescues.push(Rescue {
                classes,
                binding,
                body,
            });
        }
        let ensure = if self.eat_kw("ensure") {
            Some(self.body_until(&["end"])?)
        } else {
            None
        };
        Ok((rescues, ensure))
    }

    fn def_expr(&mut self) -> Result<Expr, String> {
        self.advance();
        // `def self.name` is a class (singleton) method.
        let singleton =
            self.is_kw("self") && matches!(&self.toks[self.pos + 1].kind, Tok::Op(o) if o == ".");
        if singleton {
            self.advance(); // self
            self.advance(); // .
        }
        let name = self.method_name()?;
        let mut params = Vec::new();
        if self.eat_op("(") {
            if !self.is_op(")") {
                params.push(self.param()?);
                while self.eat_op(",") {
                    params.push(self.param()?);
                }
            }
            self.expect_op(")")?;
        } else if !matches!(self.peek(), Tok::Newline | Tok::Semicolon) && !self.is_op("=") {
            // paren-less params (but `def name = expr` is an endless def, not a param)
            params.push(self.param()?);
            while self.eat_op(",") {
                params.push(self.param()?);
            }
        }
        // Endless method definition (Ruby 3+): `def name(params) = expression`.
        // The body is a single expression and there is no `end`.
        if self.eat_op("=") {
            let expr = self.statement()?;
            return Ok(Expr::Def {
                name,
                params,
                body: vec![expr],
                singleton,
            });
        }
        let body = self.body_with_rescue()?;
        self.expect_kw("end")?;
        Ok(Expr::Def {
            name,
            params,
            body,
            singleton,
        })
    }

    /// A method body that may carry a bare `rescue`/`ensure` (implicit `begin`),
    /// stopping at `end`. If any rescue/ensure clause is present, the body is
    /// wrapped in a `Begin` node.
    fn body_with_rescue(&mut self) -> Result<Vec<Expr>, String> {
        let body = self.body_until(&["rescue", "ensure", "end"])?;
        let (rescues, ensure) = self.rescue_tail()?;
        if rescues.is_empty() && ensure.is_none() {
            Ok(body)
        } else {
            Ok(vec![Expr::Begin {
                body,
                rescues,
                ensure,
            }])
        }
    }

    fn param(&mut self) -> Result<Param, String> {
        // `&blk` — capture the passed block as a Proc.
        if self.eat_op("&") {
            let name = self.ident_name()?;
            return Ok(Param {
                name,
                default: None,
                splat: false,
                keyword: false,
                kwsplat: false,
                block: true,
            });
        }
        // `**opts` — a keyword-splat collector.
        if self.eat_op("**") {
            let name = self.ident_name()?;
            return Ok(Param {
                name,
                default: None,
                splat: false,
                keyword: false,
                kwsplat: true,
                block: false,
            });
        }
        let splat = self.eat_op("*");
        let name = self.ident_name()?;
        // Keyword parameter: `name:` (required) or `name: default`.
        if !splat && self.is_op(":") {
            self.advance();
            let default = if self.is_op(",")
                || self.is_op(")")
                || matches!(self.peek(), Tok::Newline | Tok::Semicolon)
            {
                None
            } else {
                Some(self.ternary()?)
            };
            return Ok(Param {
                name,
                default,
                splat: false,
                keyword: true,
                kwsplat: false,
                block: false,
            });
        }
        let default = if !splat && self.eat_op("=") {
            Some(self.ternary()?)
        } else {
            None
        };
        Ok(Param {
            name,
            default,
            splat,
            keyword: false,
            kwsplat: false,
            block: false,
        })
    }

    /// `->(params) { body }` / `-> { body }` / `->(x) do … end` — a lambda.
    fn lambda_lit(&mut self) -> Result<Expr, String> {
        self.expect_op("->")?;
        let mut params = Vec::new();
        let mut splat = None;
        let mut preludes = Vec::new();
        let mut had_parens = false;
        if self.eat_op("(") {
            had_parens = true;
            if !self.is_op(")") {
                self.block_param(&mut params, &mut splat, &mut preludes)?;
                while self.eat_op(",") {
                    self.block_param(&mut params, &mut splat, &mut preludes)?;
                }
            }
            self.expect_op(")")?;
        }
        // The body is a `{ … }` or `do … end` block.
        let block = self
            .maybe_block()?
            .ok_or_else(|| format!("line {}: lambda without a body", self.line()))?;
        // Params from the `->()` header win (with its destructuring preludes
        // prepended to the body); if there were none, adopt the block's own
        // `{ |x| }` params/splat/body verbatim.
        if had_parens {
            let body = self.prepend_preludes(preludes, block.body);
            Ok(Expr::Lambda(Block {
                params,
                splat,
                body,
            }))
        } else {
            Ok(Expr::Lambda(block))
        }
    }

    fn array_lit(&mut self) -> Result<Expr, String> {
        self.expect_op("[")?;
        self.skip_terms();
        let mut items = Vec::new();
        let elem = |p: &mut Self| -> Result<Expr, String> {
            if p.is_op("*") {
                p.advance();
                Ok(Expr::Splat(Box::new(p.expr()?)))
            } else {
                p.expr()
            }
        };
        if !self.is_op("]") {
            items.push(elem(self)?);
            while self.eat_op(",") {
                self.skip_terms();
                if self.is_op("]") {
                    break;
                }
                items.push(elem(self)?);
            }
        }
        self.skip_terms();
        self.expect_op("]")?;
        Ok(Expr::Array(items))
    }

    fn hash_lit(&mut self) -> Result<Expr, String> {
        self.expect_op("{")?;
        self.skip_terms();
        let mut pairs = Vec::new();
        if !self.is_op("}") {
            pairs.push(self.hash_pair()?);
            while self.eat_op(",") {
                self.skip_terms();
                if self.is_op("}") {
                    break;
                }
                pairs.push(self.hash_pair()?);
            }
        }
        self.skip_terms();
        self.expect_op("}")?;
        Ok(Expr::Hash(pairs))
    }

    fn hash_pair(&mut self) -> Result<(Expr, Expr), String> {
        // `label: value` → symbol key; `key => value` → arbitrary key.
        if let Tok::Ident(name) = self.peek().clone() {
            // lookahead for `name:`
            if matches!(&self.toks[self.pos + 1].kind, Tok::Op(o) if o == ":") {
                self.advance();
                self.advance();
                let v = self.expr()?;
                return Ok((Expr::Symbol(name), v));
            }
        }
        let k = self.expr()?;
        self.expect_op("=>")?;
        let v = self.expr()?;
        Ok((k, v))
    }
}

/// A `while`/`until` modifier on a `begin … end` block is a post-test loop.
/// Returns `Ok(body)` — the loop body to run once before each condition check —
/// when `e` is a `begin … end`; otherwise returns the expression unchanged as
/// `Err(e)` so the caller falls back to an ordinary pre-test `while`.
///
/// A plain `begin … end` (no `rescue`/`ensure`) unwraps to its statement list.
/// A `begin … rescue/ensure … end` is kept whole as a single body statement so
/// its exception handling still fires each iteration.
fn do_while_body(e: Expr) -> Result<Vec<Expr>, Expr> {
    match e {
        Expr::Begin {
            body,
            rescues,
            ensure,
        } if rescues.is_empty() && ensure.is_none() => Ok(body),
        e @ Expr::Begin { .. } => Ok(vec![e]),
        other => Err(other),
    }
}

/// Map a comparison/bit operator string to its `BinOp`.
/// Operator tokens that may be used as method names in a `def`.
fn is_operator_method(op: &str) -> bool {
    matches!(
        op,
        "+" | "-"
            | "*"
            | "/"
            | "%"
            | "**"
            | "=="
            | "!="
            | "<"
            | ">"
            | "<="
            | ">="
            | "<=>"
            | "==="
            | "<<"
            | ">>"
            | "&"
            | "|"
            | "^"
            | "~"
    )
}

fn matchop(op: &str) -> BinOp {
    match op {
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<=>" => BinOp::Cmp,
        "=~" => BinOp::Match,
        "!~" => BinOp::NMatch,
        "===" => BinOp::CaseEq,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Mod,
        "|" => BinOp::BitOr,
        "^" => BinOp::BitXor,
        "&" => BinOp::BitAnd,
        "<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        _ => unreachable!("not a binary op: {op}"),
    }
}

/// Scan a double-quoted string body for `#{ … }` interpolation, decoding the
/// common backslash escapes in the literal segments.
fn scan_interp(raw: &str) -> Result<Vec<StrPart>, String> {
    let b = raw.as_bytes();
    let mut i = 0;
    let mut parts = Vec::new();
    let mut lit = String::new();
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let n = b[i + 1];
            match n {
                b'n' => lit.push('\n'),
                b't' => lit.push('\t'),
                b'r' => lit.push('\r'),
                b'0' => lit.push('\0'),
                b'e' => lit.push('\x1b'),
                b'a' => lit.push('\x07'),
                b'b' => lit.push('\x08'),
                b'f' => lit.push('\x0c'),
                b'v' => lit.push('\x0b'),
                b's' => lit.push(' '),
                b'\\' => lit.push('\\'),
                b'"' => lit.push('"'),
                b'#' => lit.push('#'),
                // `\xHH` — one or two hex digits → a byte.
                b'x' => {
                    let mut j = i + 2;
                    let mut val = 0u32;
                    let mut count = 0;
                    while j < b.len() && count < 2 && (b[j] as char).is_ascii_hexdigit() {
                        val = val * 16 + (b[j] as char).to_digit(16).unwrap();
                        j += 1;
                        count += 1;
                    }
                    if count > 0 {
                        lit.push(char::from_u32(val).unwrap_or('\u{fffd}'));
                        i = j;
                        continue;
                    }
                    lit.push('x');
                }
                // `\uHHHH` or `\u{H...}` — a Unicode codepoint.
                b'u' => {
                    let mut j = i + 2;
                    let mut val = 0u32;
                    if b.get(j) == Some(&b'{') {
                        j += 1;
                        while j < b.len() && (b[j] as char).is_ascii_hexdigit() {
                            val = val * 16 + (b[j] as char).to_digit(16).unwrap();
                            j += 1;
                        }
                        if b.get(j) == Some(&b'}') {
                            j += 1;
                        }
                    } else {
                        let mut count = 0;
                        while j < b.len() && count < 4 && (b[j] as char).is_ascii_hexdigit() {
                            val = val * 16 + (b[j] as char).to_digit(16).unwrap();
                            j += 1;
                            count += 1;
                        }
                    }
                    lit.push(char::from_u32(val).unwrap_or('\u{fffd}'));
                    i = j;
                    continue;
                }
                other => {
                    lit.push('\\');
                    lit.push(other as char);
                }
            }
            i += 2;
            continue;
        }
        if b[i] == b'#' && i + 1 < b.len() && b[i + 1] == b'{' {
            if !lit.is_empty() {
                parts.push(StrPart::Lit(std::mem::take(&mut lit)));
            }
            // find matching close brace (no nested-brace tracking beyond depth)
            let mut depth = 1;
            let start = i + 2;
            let mut j = start;
            while j < b.len() && depth > 0 {
                match b[j] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            if depth != 0 {
                return Err("unterminated #{ } interpolation".into());
            }
            let inner = &raw[start..j];
            let stmts = parse(inner)?;
            let e = stmts
                .into_iter()
                .last()
                .map(|s| s.expr)
                .unwrap_or(Expr::Str(vec![StrPart::Lit(String::new())]));
            parts.push(StrPart::Interp(Box::new(e)));
            i = j + 1;
            continue;
        }
        let clen = utf8_len(b[i]);
        lit.push_str(&raw[i..i + clen]);
        i += clen;
    }
    if !lit.is_empty() || parts.is_empty() {
        parts.push(StrPart::Lit(lit));
    }
    Ok(parts)
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}
