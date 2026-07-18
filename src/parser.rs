//! Recursive-descent / precedence-climbing parser for the Ruby subset rubyrs
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

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

/// Parse a full program.
pub fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
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
                e = Expr::While {
                    cond: Box::new(cond),
                    body: vec![e],
                };
            } else if self.eat_kw("until") {
                let cond = self.expr()?;
                e = Expr::While {
                    cond: Box::new(Expr::Unary(UnOp::Not, Box::new(cond))),
                    body: vec![e],
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
        let lo = self.binary(0)?;
        if self.is_op("..") || self.is_op("...") {
            let exclusive = self.is_op("...");
            self.advance();
            let hi = self.binary(0)?;
            return Ok(Expr::Range {
                lo: Box::new(lo),
                hi: Box::new(hi),
                exclusive,
            });
        }
        Ok(lo)
    }

    /// Binding power of a binary operator token, or None if not a binary op.
    fn bp(op: &str) -> Option<(u8, BinOp)> {
        Some(match op {
            "||" => (1, BinOp::Or),
            "&&" => (2, BinOp::And),
            "==" | "!=" | "<=>" | "===" => (3, matchop(op)),
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
            if self.eat_op(".") || self.eat_op("&.") {
                let name = self.method_name()?;
                let (args, block) = self.call_tail()?;
                e = Expr::Call {
                    recv: Some(Box::new(e)),
                    name,
                    args,
                    block,
                };
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
            other => Err(format!(
                "line {}: expected method name, found '{}'",
                self.line(),
                other
            )),
        }
    }

    /// Parse the argument list + optional block that follow a method name.
    fn call_tail(&mut self) -> Result<(Vec<Expr>, Option<Block>), String> {
        let mut args = Vec::new();
        if self.eat_op("(") {
            if !self.is_op(")") {
                args.push(self.arg()?);
                while self.eat_op(",") {
                    args.push(self.arg()?);
                }
            }
            self.expect_op(")")?;
        }
        let block = self.maybe_block()?;
        Ok((args, block))
    }

    /// An argument allows `key: val` and `key => val` pair sugar (collapsed into
    /// a trailing hash by the caller is out of scope; here we just parse a value).
    fn arg(&mut self) -> Result<Expr, String> {
        self.expr()
    }

    fn maybe_block(&mut self) -> Result<Option<Block>, String> {
        if self.eat_kw("do") {
            let params = self.block_params()?;
            let body = self.body_until(&["end"])?;
            self.expect_kw("end")?;
            return Ok(Some(Block { params, body }));
        }
        if self.is_op("{") {
            self.advance();
            let params = self.block_params()?;
            let mut body = Vec::new();
            self.skip_terms();
            while !self.is_op("}") && !matches!(self.peek(), Tok::Eof) {
                body.push(self.statement()?);
                self.skip_terms();
            }
            self.expect_op("}")?;
            return Ok(Some(Block { params, body }));
        }
        Ok(None)
    }

    fn block_params(&mut self) -> Result<Vec<String>, String> {
        let mut params = Vec::new();
        if self.eat_op("|") {
            if !self.is_op("|") {
                params.push(self.ident_name()?);
                while self.eat_op(",") {
                    params.push(self.ident_name()?);
                }
            }
            self.expect_op("|")?;
        }
        Ok(params)
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
            Tok::Symbol(s) => {
                self.advance();
                Ok(Expr::Symbol(s))
            }
            Tok::IVar(s) => {
                self.advance();
                Ok(Expr::Var(VarKind::Instance, s))
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
        // (`puts x`, `puts (a).b`, `puts [1,2]`).
        if self.cur_space() && self.starts_command_arg() {
            let mut args = vec![self.arg()?];
            while self.eat_op(",") {
                args.push(self.arg()?);
            }
            let block = self.maybe_block()?;
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
            | Tok::Symbol(_)
            | Tok::IVar(_)
            | Tok::GVar(_)
            | Tok::Const(_)
            | Tok::Ident(_) => true,
            Tok::Keyword(k) => matches!(k.as_str(), "nil" | "true" | "false" | "self"),
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
                Ok(Expr::Var(VarKind::Local, "self".into()))
            }
            "if" => self.if_expr(false),
            "unless" => self.if_expr(true),
            "while" => self.while_expr(false),
            "until" => self.while_expr(true),
            "for" => self.for_expr(),
            "case" => self.case_expr(),
            "def" => self.def_expr(),
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
            "begin" => {
                // minimal begin/end (rescue/ensure parsed and dropped for now)
                self.advance();
                let body = self.body_until(&["end", "rescue", "ensure"])?;
                while self.is_kw("rescue") || self.is_kw("ensure") {
                    self.advance();
                    let _ = self.body_until(&["end", "rescue", "ensure"])?;
                }
                self.expect_kw("end")?;
                Ok(block_value(body))
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

    fn def_expr(&mut self) -> Result<Expr, String> {
        self.advance();
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
        } else if !matches!(self.peek(), Tok::Newline | Tok::Semicolon) {
            // paren-less params
            params.push(self.param()?);
            while self.eat_op(",") {
                params.push(self.param()?);
            }
        }
        let body = self.body_until(&["end"])?;
        self.expect_kw("end")?;
        Ok(Expr::Def { name, params, body })
    }

    fn param(&mut self) -> Result<Param, String> {
        let name = self.ident_name()?;
        let default = if self.eat_op("=") {
            Some(self.ternary()?)
        } else {
            None
        };
        Ok(Param { name, default })
    }

    fn array_lit(&mut self) -> Result<Expr, String> {
        self.expect_op("[")?;
        self.skip_terms();
        let mut items = Vec::new();
        if !self.is_op("]") {
            items.push(self.expr()?);
            while self.eat_op(",") {
                self.skip_terms();
                if self.is_op("]") {
                    break;
                }
                items.push(self.expr()?);
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

/// Map a comparison/bit operator string to its `BinOp`.
fn matchop(op: &str) -> BinOp {
    match op {
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<=>" => BinOp::Cmp,
        "===" => BinOp::Eq,
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

/// Wrap a body so it yields its last expression's value (used by `begin`/`end`).
fn block_value(mut body: Vec<Expr>) -> Expr {
    if body.len() == 1 {
        body.pop().unwrap()
    } else {
        // fabricate an if-true wrapper that evaluates the sequence
        Expr::If {
            cond: Box::new(Expr::True),
            then: body,
            elifs: vec![],
            els: None,
        }
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
                b's' => lit.push(' '),
                b'\\' => lit.push('\\'),
                b'"' => lit.push('"'),
                b'#' => lit.push('#'),
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
