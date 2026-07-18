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
//! Not yet lexed (tracked in BUGS.md): heredocs, `%w[]`/`%i[]` literals, regex
//! literals, `?c` character literals. Double-quoted `#{…}` interpolation IS
//! handled — the interpolating scan is done in the parser from the raw string
//! body this lexer captures.

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
    Symbol(String),
    Ident(String),
    Const(String), // capitalized identifier
    IVar(String),  // @foo
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
            Tok::Symbol(s) => write!(f, ":{s}"),
            Tok::Ident(s) | Tok::Const(s) | Tok::Keyword(s) => write!(f, "{s}"),
            Tok::IVar(s) => write!(f, "@{s}"),
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
    "class", "module", "begin", "rescue", "ensure", "self",
];

/// Tokenize `src`. Returns an error string on an unterminated string or an
/// unexpected byte.
pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut out: Vec<Token> = Vec::new();
    // Whether whitespace has been seen since the last emitted token.
    let mut sp = true;

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
                if !continues(&out) {
                    out.push(Token {
                        kind: Tok::Newline,
                        line,
                        space: core::mem::take(&mut sp),
                    });
                }
                sp = true;
                line += 1;
                i += 1;
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
                    out.push(Token {
                        kind: Tok::Int(n),
                        line,
                        space: core::mem::take(&mut sp),
                    });
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
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Token {
                    kind: Tok::IVar(src[start..i].to_string()),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            b'$' => {
                i += 1;
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Token {
                    kind: Tok::GVar(src[start..i].to_string()),
                    line,
                    space: core::mem::take(&mut sp),
                });
            }
            b':' if i + 1 < b.len() && (b[i + 1].is_ascii_alphabetic() || b[i + 1] == b'_') => {
                i += 1;
                let start = i;
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
            _ => {
                // operators / punctuation, longest match first
                let three = if i + 3 <= b.len() { &src[i..i + 3] } else { "" };
                let two = if i + 2 <= b.len() { &src[i..i + 2] } else { "" };
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
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}
