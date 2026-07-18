//! Ruby lexer.
//!
//! Produces a flat `Vec<Token>` the parser consumes. Newlines are significant in
//! Ruby (they terminate statements) so they surface as `Token::Newline` rather
//! than being skipped; the parser treats a newline and a `;` identically. Line
//! continuation with a trailing `\`, and newlines right after a binary operator
//! or a `,`/`.`, are swallowed here so the parser never sees a spurious
//! terminator mid-expression — matching Ruby's own "expression clearly
//! continues" rule.
//!
//! Heredocs (`<<END`, `<<~SQL`, `<<-EOT`, `<<'RAW'`), regex literals, `?c`
//! character literals, and `%w[]`/`%i[]` word/symbol arrays are all lexed.
//! Double-quoted `#{…}` interpolation IS handled — the interpolating scan is
//! done in the parser from the raw string body this lexer captures.

use std::fmt;

/// A lexical token with its source line (1-based) for diagnostics and whether
/// whitespace immediately preceded it — Ruby needs the space to disambiguate
/// `foo(x)` (call) from `foo (x)` (command arg) and `h[k]` (index) from
/// `puts [x]` (command arg).
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub line: u32,
    pub space: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    /// Raw string body plus whether it was double-quoted (interpolation allowed).
    Str(String, bool),
    /// A regex literal: `/pattern/flags` — (pattern, flags).
    Regex(String, String),
    Symbol(String),
    Ident(String),
    Const(String), // capitalized identifier
    IVar(String),  // @foo
    CVar(String),  // @@foo
    GVar(String),  // $foo
    Keyword(String),
    Op(String),
    Newline,
    Semicolon,
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Int(n) => write!(f, "{n}"),
            Tok::Float(x) => write!(f, "{x}"),
            Tok::Str(s, _) => write!(f, "\"{s}\""),
            Tok::Regex(s, fl) => write!(f, "/{s}/{fl}"),
            Tok::Symbol(s) => write!(f, ":{s}"),
            Tok::Ident(s) | Tok::Const(s) | Tok::Keyword(s) => write!(f, "{s}"),
            Tok::IVar(s) => write!(f, "@{s}"),
            Tok::CVar(s) => write!(f, "@@{s}"),
            Tok::GVar(s) => write!(f, "${s}"),
            Tok::Op(s) => write!(f, "{s}"),
            Tok::Newline => write!(f, "\\n"),
            Tok::Semicolon => write!(f, ";"),
            Tok::Eof => write!(f, "<eof>"),
        }
    }
}

const KEYWORDS: &[&str] = &[
    "def", "end", "if", "elsif", "else", "unless", "while", "until", "for", "in", "do", "return",
    "break", "next", "yield", "then", "case", "when", "nil", "true", "false", "and", "or", "not",
    "class", "module", "begin", "rescue", "ensure", "self", "super", "retry", "alias",
];

/// Tokenize `src`. Returns an error string on an unterminated string or an
/// unexpected byte.
/// A heredoc marker seen mid-line whose body is collected once the line ends.
struct Heredoc {
    delim: String,
    /// `<<~` — strip the common leading indentation from the body.
    squiggly: bool,
    /// `<<-` or `<<~` — the terminator line may be indented.
    indent_term: bool,
    /// Whether the body interpolates (`<<END`/`<<"END"`/`<<~`, but not `<<'END'`).
    interp: bool,
    /// Index in the token stream of the placeholder `Str` token to fill in.
    tok_idx: usize,
}

/// Collect a heredoc body starting at byte offset `start` (the first char after
/// the marker line's newline) up to a line equal to `delim`. Returns the raw
/// body, the offset just past the terminator line, and the number of newlines
/// consumed. `indent_term` allows the terminator to have leading whitespace.
fn collect_heredoc_body(
    b: &[u8],
    start: usize,
    delim: &str,
    indent_term: bool,
) -> (String, usize, u32) {
    let mut body = String::new();
    let mut pos = start;
    let mut lines = 0u32;
    while pos < b.len() {
        let line_start = pos;
        while pos < b.len() && b[pos] != b'\n' {
            pos += 1;
        }
        let line = std::str::from_utf8(&b[line_start..pos]).unwrap_or("");
        let trimmed = if indent_term { line.trim_start() } else { line };
        // Consume the newline (if any) that ends this line.
        let had_nl = pos < b.len();
        if had_nl {
            pos += 1;
            lines += 1;
        }
        if trimmed == delim {
            return (body, pos, lines);
        }
        body.push_str(line);
        body.push('\n');
    }
    // Unterminated: treat the rest of the input as the body.
    (body, pos, lines)
}

/// `<<~` squiggly heredoc: strip the least-indented line's leading whitespace
/// from every line.
fn strip_squiggly(body: &str) -> String {
    let indent = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = String::new();
    for line in body.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        if content.len() >= indent {
            out.push_str(&content[indent..]);
        }
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Whether the next non-blank line (skipping whitespace, blank lines, and
/// comments) begins with a `.method` call — a leading-dot chain continuation.
/// `..`/`...` (a range) does not count.
fn next_line_leading_dot(b: &[u8], mut j: usize) -> bool {
    loop {
        while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        // Skip a comment line.
        if j < b.len() && b[j] == b'#' {
            while j < b.len() && b[j] != b'\n' {
                j += 1;
            }
            continue;
        }
        break;
    }
    b.get(j) == Some(&b'.') && b.get(j + 1) != Some(&b'.')
}

/// Whether a `<<` here begins a heredoc rather than a left-shift: true after a
/// value is NOT on the stack (start of expression, after an operator/`(`/`,`),
/// or whenever an unambiguous form (`<<~`/`<<-`/`<<"`/`<<'`) is used.
fn heredoc_here(out: &[Token], after: &[u8]) -> bool {
    let unambiguous = matches!(
        after.first(),
        Some(b'~') | Some(b'-') | Some(b'"') | Some(b'\'')
    );
    if unambiguous {
        return true;
    }
    // A bare `<<IDENT` heredoc must start with an uppercase letter or `_` and
    // appear in value position (not after a value that would make it a shift).
    let bare_ok = matches!(after.first(), Some(c) if c.is_ascii_uppercase() || *c == b'_');
    if !bare_ok {
        return false;
    }
    match out.iter().rev().find(|t| t.kind != Tok::Newline) {
        None => true,
        Some(t) => {
            matches!(
                &t.kind,
                Tok::Op(o) if o != ")" && o != "]" && o != "}"
            ) || matches!(&t.kind, Tok::Keyword(_))
        }
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut out: Vec<Token> = Vec::new();
    // Whether whitespace has been seen since the last emitted token.
    let mut sp = true;
    // Heredoc markers seen on the current line, whose bodies are collected when
    // the line's newline is reached.
    let mut pending: Vec<Heredoc> = Vec::new();

    // Whether a newline here continues the previous expression (last real token
    // is a binary op / comma / dot / open bracket / `=`).
    let continues = |out: &[Token]| -> bool {
        match out.iter().rev().find(|t| t.kind != Tok::Newline) {
            None => true,
            Some(t) => {
                matches!(&t.kind,
                Tok::Op(o) if o != ")" && o != "]" && o != "}" )
                    || matches!(&t.kind, Tok::Keyword(k)
                    if matches!(k.as_str(), "and"|"or"|"not"|"if"|"unless"|"while"|"until"|"do"|"then"|"else"|"in"))
            }
        }
    };

    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' => {
                sp = true;
                i += 1;
            }
            b'\\' if i + 1 < b.len() && b[i + 1] == b'\n' => {
                // explicit line continuation
                sp = true;
                line += 1;
                i += 2;
            }
            b'\n' => {
                // A newline is swallowed when the previous line clearly continues
                // (trailing operator) OR the next line starts with `.method` (a
                // leading-dot method chain).
                if !continues(&out) && !next_line_leading_dot(b, i + 1) {
                    out.push(Token {
                        kind: Tok::Newline,
                        line,
                        space: core::mem::take(&mut sp),
                    });
                }
                sp = true;
                line += 1;
                i += 1;
                // Collect the bodies of any heredocs opened on this line, in
                // order, from the lines that follow.
                if !pending.is_empty() {
                    for hd in pending.drain(..) {
                        let (raw, next, lines) =
                            collect_heredoc_body(b, i, &hd.delim, hd.indent_term);
                        let body = if hd.squiggly {
                            strip_squiggly(&raw)
                        } else {
                            raw
                        };
                        out[hd.tok_idx].kind = Tok::Str(body, hd.interp);
                        i = next;
                        line += lines;
                    }
                }
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                sp = true;
            }
            b';' => {
                out.push(Token {
                    kind: Tok::Semicolon,
                    line,
                    space: core::mem::take(&mut sp),
                });
                i += 1;
            }
            b'0'..=b'9' => {
                // Radix-prefixed integer literals: `0b1010` (binary), `0o17` /
                // `017` (octal), `0xff` (hex), `0d99` (decimal). A leading `0`
                // followed by octal digits is octal, matching Ruby.
                if b[i] == b'0' && i + 1 < b.len() {
                    let (radix, skip) = match b[i + 1] {
                        b'b' | b'B' => (2u32, 2),
                        b'o' | b'O' => (8, 2),
                        b'x' | b'X' => (16, 2),
                        b'd' | b'D' => (10, 2),
                        c if c.is_ascii_digit() => (8, 1),
                        _ => (0, 0),
                    };
                    if radix != 0 {
                        let dstart = i + skip;
                        let mut j = dstart;
                        while j < b.len() {
                            let c = b[j] as char;
                            if c == '_' || c.is_digit(radix) {
                                j += 1;
                            } else {
                                break;
                            }
                        }
                        let digits: String = src[dstart..j].chars().filter(|c| *c != '_').collect();
                        let n = i64::from_str_radix(&digits, radix)
                            .map_err(|_| format!("bad integer literal: {}", &src[i..j]))?;
                        out.push(Token {
                            kind: Tok::Int(n),
                            line,
                            space: core::mem::take(&mut sp),
                        });
                        i = j;
                        continue;
                    }
                }
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
                let mut is_float = false;
                if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                    is_float = true;
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                        i += 1;
                    }
                }
                if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                    is_float = true;
                    i += 1;
                    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                        i += 1;
                    }
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let raw: String = src[start..i].chars().filter(|c| *c != '_').collect();
                if is_float {
                    let x: f64 = raw.parse().map_err(|_| format!("bad float: {raw}"))?;
                    out.push(Token {
                        kind: Tok::Float(x),
                        line,
                        space: core::mem::take(&mut sp),
                    });
                } else {
                    let n: i64 = raw.parse().map_err(|_| format!("bad integer: {raw}"))?;
                    // A trailing `r`/`i` (not part of an identifier) makes a
                    // Rational (`4r` → `Rational(4)`) or imaginary Complex
                    // (`3i` → `Complex(0, 3)`) literal, desugared into a call.
                    let suffix = b
                        .get(i)
                        .filter(|c| {
                            (**c == b'r' || **c == b'i')
                                && !b
                                    .get(i + 1)
                                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                        })
                        .copied();
                    let this_sp = core::mem::take(&mut sp);
                    let synth: Vec<Tok> = match suffix {
                        Some(b'r') => vec![
                            Tok::Ident("Rational".into()),
                            Tok::Op("(".into()),
                            Tok::Int(n),
                            Tok::Op(")".into()),
                        ],
                        Some(b'i') => vec![
                            Tok::Ident("Complex".into()),
                            Tok::Op("(".into()),
                            Tok::Int(0),
                            Tok::Op(",".into()),
                            Tok::Int(n),
                            Tok::Op(")".into()),
                        ],
                        _ => vec![Tok::Int(n)],
                    };
                    if suffix.is_some() {
                        i += 1;
                    }
                    for (k, kind) in synth.into_iter().enumerate() {
                        out.push(Token {
                            kind,
                            line,
                            space: if k == 0 { this_sp } else { false },
                        });
                    }
                }
            }
            b'"' | b'\'' => {
                let quote = c;
                let dq = quote == b'"';
                i += 1;
                let mut s = String::new();
                loop {
                    if i >= b.len() {
                        return Err(format!("unterminated string at line {line}"));
                    }
                    let ch = b[i];
                    if ch == quote {
                        i += 1;
                        break;
                    }
                    if ch == b'\\' && i + 1 < b.len() {
                        // Keep escapes raw for double-quoted (the parser's interp
                        // scan decodes them); decode the essentials for single.
                        let n = b[i + 1];
                        if dq {
                            s.push('\\');
                            s.push(n as char);
                        } else {
                            match n {
                                b'\'' => s.push('\''),
                                b'\\' => s.push('\\'),
                                other => {
                                    s.push('\\');
                                    s.push(other as char);
                                }
                            }
                        }
                        i += 2;
                        continue;
                    }
                    // A `#{ … }` interpolation in a double-quoted string is copied
                    // verbatim (the parser re-scans it), balancing braces and any
                    // nested string literals so an inner `"` doesn't end us.
                    if dq && ch == b'#' && i + 1 < b.len() && b[i + 1] == b'{' {
                        s.push('#');
                        s.push('{');
                        i += 2;
                        let mut depth = 1u32;
                        while i < b.len() && depth > 0 {
                            let d = b[i];
                            match d {
                                b'{' => depth += 1,
                                b'}' => depth -= 1,
                                b'"' | b'\'' => {
                                    // skip a nested string literal wholesale
                                    let q = d;
                                    s.push(q as char);
                                    i += 1;
                                    while i < b.len() && b[i] != q {
                                        if b[i] == b'\\' && i + 1 < b.len() {
                                            s.push('\\');
                                            s.push(b[i + 1] as char);
                                            i += 2;
                                            continue;
                                        }
                                        if b[i] == b'\n' {
                                            line += 1;
                                        }
                                        let cl = utf8_len(b[i]);
                                        s.push_str(&src[i..i + cl]);
                                        i += cl;
                                    }
                                    if i < b.len() {
                                        s.push(q as char);
                                        i += 1;
                                    }
                                    continue;
                                }
                                b'\n' => line += 1,
                                _ => {}
                            }
                            if depth == 0 {
                                s.push('}');
                                i += 1;
                                break;
                            }
                            let cl = utf8_len(d);
                            s.push_str(&src[i..i + cl]);
                            i += cl;
                        }
                        continue;
                    }
                    if ch == b'\n' {
                        line += 1;
                    }
                    // advance one UTF-8 char
                    let clen = utf8_len(ch);
                    s.push_str(&src[i..i + clen]);
                    i += clen;
                }
                out.push(Token {
                    kind: Tok::Str(s, dq),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            b'@' => {
                i += 1;
                // `@@name` is a class variable; `@name` an instance variable.
                let class_var = i < b.len() && b[i] == b'@';
                if class_var {
                    i += 1;
                }
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let name = src[start..i].to_string();
                out.push(Token {
                    kind: if class_var {
                        Tok::CVar(name)
                    } else {
                        Tok::IVar(name)
                    },
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            b'$' => {
                i += 1;
                let start = i;
                // Special single-punctuation globals: `$~` (last MatchData),
                // `$&` (whole match), `` $` `` (pre-match), `$'` (post-match),
                // `$+` (last group), `$,` (output field separator), `$\` (output
                // record separator), `$;` (input field separator), `$/` (input
                // record separator), `$!` (last error). Otherwise an
                // alphanumeric/underscore name.
                if i < b.len()
                    && matches!(
                        b[i],
                        b'~' | b'&' | b'`' | b'\'' | b'+' | b',' | b'\\' | b';' | b'/' | b'!'
                    )
                {
                    i += 1;
                } else {
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                }
                out.push(Token {
                    kind: Tok::GVar(src[start..i].to_string()),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            // `%w[…]` / `%i[…]` word/symbol arrays → synthetic `[ … ]` tokens so
            // the parser handles them as ordinary array literals.
            b'%' if i + 2 < b.len()
                && matches!(b[i + 1], b'w' | b'i' | b'W' | b'I')
                && matches!(b[i + 2], b'[' | b'(' | b'{' | b'<' | b'|') =>
            {
                let symbols = matches!(b[i + 1], b'i' | b'I');
                let open = b[i + 2];
                let close = match open {
                    b'[' => b']',
                    b'(' => b')',
                    b'{' => b'}',
                    b'<' => b'>',
                    other => other,
                };
                i += 3;
                let start = i;
                let mut depth = 1u32;
                while i < b.len() {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    if b[i] == open && open != close {
                        depth += 1;
                    } else if b[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                let body = &src[start..i.min(b.len())];
                i += 1; // consume the closing delimiter
                let words: Vec<&str> = body.split_whitespace().collect();
                out.push(Token {
                    kind: Tok::Op("[".into()),
                    line,
                    space: core::mem::take(&mut sp),
                });
                for (idx, w) in words.iter().enumerate() {
                    if idx > 0 {
                        out.push(Token {
                            kind: Tok::Op(",".into()),
                            line,
                            space: false,
                        });
                    }
                    let kind = if symbols {
                        Tok::Symbol((*w).to_string())
                    } else {
                        Tok::Str((*w).to_string(), false)
                    };
                    out.push(Token {
                        kind,
                        line,
                        space: false,
                    });
                }
                out.push(Token {
                    kind: Tok::Op("]".into()),
                    line,
                    space: false,
                });
            }
            // `?c` character literal → a one-char string. Only when `?` is at an
            // expression start (leading space) and directly followed by an escape
            // (`?\n`) or a lone alnum char (not `?ab`, and not the `? :` ternary).
            b'?' if sp
                && i + 1 < b.len()
                && (b[i + 1] == b'\\'
                    || (b[i + 1].is_ascii_alphanumeric()
                        && (i + 2 >= b.len()
                            || !(b[i + 2].is_ascii_alphanumeric() || b[i + 2] == b'_')))) =>
            {
                i += 1;
                let ch = if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                    let e = b[i];
                    i += 1;
                    match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b's' => ' ',
                        b'0' => '\0',
                        b'e' => '\x1b',
                        other => other as char,
                    }
                } else {
                    let c = b[i] as char;
                    i += 1;
                    c
                };
                out.push(Token {
                    kind: Tok::Str(ch.to_string(), false),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            // Operator symbols: `:+`, `:<=>`, `:[]`, … only in value position (so
            // `a ? b : c` ternary and `key: v` hash keys are untouched).
            // An operator symbol (`:+`, `:<<`, `:[]`) — recognized when it can't be
            // a ternary colon: either at the start of an expression, or as a
            // spaced command argument (`foo :<<`). Ternary's `:` is followed by a
            // space, so `op_symbol_at` never matches there.
            b':' if i + 1 < b.len()
                && op_symbol_at(&src[i + 1..]).is_some()
                && (!prev_is_value(&out) || sp) =>
            {
                let op = op_symbol_at(&src[i + 1..]).unwrap();
                i += 1 + op.len();
                out.push(Token {
                    kind: Tok::Symbol(op.to_string()),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            b':' if i + 1 < b.len()
                && (b[i + 1].is_ascii_alphabetic() || b[i + 1] == b'_' || b[i + 1] == b'@') =>
            {
                i += 1;
                let start = i;
                // Instance/class-variable symbols keep their sigil: `:@x`, `:@@x`.
                while i < b.len() && b[i] == b'@' {
                    i += 1;
                }
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                // allow trailing ? / ! on symbol/method names
                if i < b.len() && (b[i] == b'?' || b[i] == b'!') {
                    i += 1;
                }
                out.push(Token {
                    kind: Tok::Symbol(src[start..i].to_string()),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                // method-name suffixes ? and !
                if i < b.len() && (b[i] == b'?' || b[i] == b'!') {
                    i += 1;
                }
                let word = &src[start..i];
                let kind = if KEYWORDS.contains(&word) {
                    Tok::Keyword(word.to_string())
                } else if word.as_bytes()[0].is_ascii_uppercase() {
                    Tok::Const(word.to_string())
                } else {
                    Tok::Ident(word.to_string())
                };
                out.push(Token {
                    kind,
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            // `/pattern/flags` regex literal — when `/` is at an expression start
            // (not a division / `/=`). Body respects `\/` escapes and `[…]`
            // char-classes; trailing `imx` are flags.
            b'/' if regex_start(&out, sp, b.get(i + 1).copied()) => {
                i += 1;
                let mut pat = String::new();
                let mut in_class = false;
                while i < b.len() {
                    let ch = b[i];
                    if ch == b'\\' && i + 1 < b.len() {
                        pat.push('\\');
                        pat.push(b[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    match ch {
                        b'[' => in_class = true,
                        b']' => in_class = false,
                        b'/' if !in_class => break,
                        b'\n' => line += 1,
                        _ => {}
                    }
                    let cl = utf8_len(ch);
                    pat.push_str(&src[i..i + cl]);
                    i += cl;
                }
                i += 1; // consume closing `/`
                let fstart = i;
                while i < b.len() && matches!(b[i], b'i' | b'm' | b'x' | b'o' | b'u' | b'n') {
                    i += 1;
                }
                let flags = src[fstart..i].to_string();
                out.push(Token {
                    kind: Tok::Regex(pat, flags),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            b'<' if i + 2 < b.len() && b[i + 1] == b'<' && heredoc_here(&out, &b[i + 2..]) => {
                // A heredoc marker: `<<DELIM`, `<<~DELIM`, `<<-DELIM`, or a quoted
                // delimiter. Consume the marker; the body is filled in when the
                // line's newline is reached.
                let mut j = i + 2;
                let squiggly = b[j] == b'~';
                let dash = b[j] == b'-';
                if squiggly || dash {
                    j += 1;
                }
                let (quote, interp) = match b.get(j) {
                    Some(b'\'') => (Some(b'\''), false),
                    Some(b'"') => (Some(b'"'), true),
                    Some(b'`') => (Some(b'`'), true),
                    _ => (None, true),
                };
                if quote.is_some() {
                    j += 1;
                }
                let delim_start = j;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                let delim = src[delim_start..j].to_string();
                if let Some(q) = quote {
                    if b.get(j) == Some(&q) {
                        j += 1;
                    }
                }
                // Emit a placeholder Str token (filled at end of line) and record
                // the pending heredoc.
                out.push(Token {
                    kind: Tok::Str(String::new(), interp),
                    line,
                    space: core::mem::take(&mut sp),
                });
                pending.push(Heredoc {
                    delim,
                    squiggly,
                    indent_term: squiggly || dash,
                    interp,
                    tok_idx: out.len() - 1,
                });
                i = j;
            }
            _ => {
                // operators / punctuation, longest match first. Guard the string
                // slices on char boundaries: a multibyte char elsewhere on the
                // line means `i + n` can land inside a codepoint, and slicing a
                // `str` off a boundary panics.
                let three = if i + 3 <= b.len() && src.is_char_boundary(i + 3) {
                    &src[i..i + 3]
                } else {
                    ""
                };
                let two = if i + 2 <= b.len() && src.is_char_boundary(i + 2) {
                    &src[i..i + 2]
                } else {
                    ""
                };
                let op: &str = if matches!(
                    three,
                    "**=" | "<=>" | "===" | "..." | "&&=" | "||=" | "<<=" | ">>="
                ) {
                    three
                } else if matches!(
                    two,
                    "**" | "=="
                        | "!="
                        | "<="
                        | ">="
                        | "&&"
                        | "||"
                        | "+="
                        | "-="
                        | "*="
                        | "/="
                        | "%="
                        | "<<"
                        | ">>"
                        | ".."
                        | "::"
                        | "=>"
                        | "|="
                        | "&="
                        | "^="
                        | "->"
                        | "=~"
                        | "!~"
                        | "&."
                ) {
                    two
                } else {
                    match c {
                        b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&'
                        | b'|' | b'^' | b'~' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b','
                        | b'.' | b':' | b'?' => &src[i..i + 1],
                        other => {
                            return Err(format!(
                                "unexpected character '{}' at line {line}",
                                other as char
                            ))
                        }
                    }
                };
                out.push(Token {
                    kind: Tok::Op(op.to_string()),
                    line,
                    space: core::mem::take(&mut sp),
                });
                i += op.len();
            }
        }
    }
    out.push(Token {
        kind: Tok::Eof,
        line,
        space: core::mem::take(&mut sp),
    });
    Ok(out)
}

/// Byte length of the UTF-8 sequence whose lead byte is `b`.
/// Whether a `/` here begins a regex literal rather than division. It does when
/// the previous real token is not a value (so `/` sits at an expression start),
/// or — after a value — when there's a space before `/` but not after (the
/// command-argument form `scan /re/`, versus `a / b` division).
fn regex_start(out: &[Token], sp: bool, next: Option<u8>) -> bool {
    // `/=` is always the divide-assign operator, never a regex.
    if next == Some(b'=') {
        return false;
    }
    let prev = out.iter().rev().find(|t| t.kind != Tok::Newline);
    let prev_is_value = match prev.map(|t| &t.kind) {
        None => false,
        Some(Tok::Int(_))
        | Some(Tok::Float(_))
        | Some(Tok::Str(_, _))
        | Some(Tok::Regex(_, _))
        | Some(Tok::Ident(_))
        | Some(Tok::Const(_))
        | Some(Tok::IVar(_))
        | Some(Tok::GVar(_))
        | Some(Tok::Symbol(_)) => true,
        Some(Tok::Op(o)) => o == ")" || o == "]" || o == "}",
        Some(Tok::Keyword(k)) => matches!(k.as_str(), "self" | "end" | "true" | "false" | "nil"),
        _ => false,
    };
    if !prev_is_value {
        return true;
    }
    // After a value: a command-argument regex needs a leading space and no space
    // immediately after the `/` (so `foo /re/` is a regex, `a / b` is division).
    sp && !matches!(next, Some(b' ') | Some(b'\t') | None)
}

/// Operator method-name symbols, longest-first so `starts_with` picks the
/// longest (`[]=` before `[]`, `<=>` before `<=` before `<`).
const OP_SYMBOLS: &[&str] = &[
    "<=>", "===", "[]=", "**", "==", "!=", "<=", ">=", "<<", ">>", "=~", "[]", "+", "-", "*", "/",
    "%", "<", ">", "&", "|", "^", "~", "!",
];

/// The operator symbol beginning `s` (text right after `:`), or `None`.
fn op_symbol_at(s: &str) -> Option<&'static str> {
    OP_SYMBOLS.iter().find(|op| s.starts_with(**op)).copied()
}

/// Whether the last significant token is a value (so a following `:op` is a
/// ternary colon / hash key, not an operator symbol).
fn prev_is_value(out: &[Token]) -> bool {
    let prev = out.iter().rev().find(|t| t.kind != Tok::Newline);
    match prev.map(|t| &t.kind) {
        Some(Tok::Int(_))
        | Some(Tok::Float(_))
        | Some(Tok::Str(_, _))
        | Some(Tok::Regex(_, _))
        | Some(Tok::Ident(_))
        | Some(Tok::Const(_))
        | Some(Tok::IVar(_))
        | Some(Tok::GVar(_))
        | Some(Tok::Symbol(_)) => true,
        Some(Tok::Op(o)) => o == ")" || o == "]" || o == "}",
        Some(Tok::Keyword(k)) => matches!(k.as_str(), "self" | "end" | "true" | "false" | "nil"),
        _ => false,
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}
