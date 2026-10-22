//! Port of MRI's `ERB::Compiler` (erb 6.0.7, `lib/erb/compiler.rb`): the
//! scanner family, every trim mode (`nil`/`0`, `1`/`">"`, `2`/`"<>"`, `"-"`,
//! and the `"%"` line prefix), the magic-comment probe and the `Buffer` that
//! assembles the Ruby source `ERB#src` answers and `ERB#result` evaluates.
//!
//! The output is MRI's byte for byte on ASCII templates: `_erbout = +'';
//! _erbout.<< "text".freeze; _erbout.<<((expr).to_s)`, one `; `-joined line
//! per template line so a diagnostic inside the template reports the right
//! line. MRI compiles the template as bytes (`String#b`) and `dump`s them, so
//! its `src` spells a non-ASCII character as `\xNN` escapes; this port keeps
//! the character itself. The evaluated result is identical either way.

/// The scanner a template is compiled with — the second half of
/// `prepare_trim_mode`'s answer (`@trim_mode`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trim {
    /// `nil`/`0`: no trimming.
    None,
    /// `1`/`">"`: a newline right after `%>` is dropped.
    Gt,
    /// `2`/`"<>"`: that newline is dropped only when the line began with a tag.
    LtGt,
    /// `"-"`: `<%-` strips leading blanks on its line, `-%>` drops the newline.
    Dash,
}

/// `ERB.new`'s `trim_mode:` argument as the caller passed it.
#[derive(Debug)]
pub enum TrimArg {
    Nil,
    Int(i64),
    Str(String),
    /// Anything else, carried as its `inspect` for the warning text.
    Other(String),
}

/// What `prepare_trim_mode` derives: `@percent` and `@trim_mode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TrimSpec {
    pub percent: bool,
    pub trim: Trim,
}

/// `ERB::Compiler#prepare_trim_mode`. Answers the spec plus the warning MRI
/// issues for a mode it does not recognise (the caller decides where it goes).
pub fn prepare_trim_mode(arg: &TrimArg) -> (TrimSpec, Option<String>) {
    let spec = |percent, trim| TrimSpec { percent, trim };
    let warning = |shown: &str| {
        Some(format!(
            "Invalid ERB trim mode: {shown} (trim_mode: nil, 0, 1, 2, or String composed of '%' and/or '-', '>', '<>')"
        ))
    };
    match arg {
        TrimArg::Int(1) => (spec(false, Trim::Gt), None),
        TrimArg::Int(2) => (spec(false, Trim::LtGt), None),
        TrimArg::Int(0) | TrimArg::Nil => (spec(false, Trim::None), None),
        TrimArg::Str(mode) => {
            let warn = if valid_trim_mode_string(mode) {
                None
            } else {
                warning(&format!("{mode:?}"))
            };
            let percent = mode.contains('%');
            let trim = if mode.contains('-') {
                Trim::Dash
            } else if mode.contains("<>") {
                Trim::LtGt
            } else if mode.contains('>') {
                Trim::Gt
            } else {
                Trim::None
            };
            (spec(percent, trim), warn)
        }
        TrimArg::Int(n) => (spec(false, Trim::None), warning(&n.to_string())),
        TrimArg::Other(inspect) => (spec(false, Trim::None), warning(inspect)),
    }
}

/// `mode.match?(/\A(%|-|>|<>){1,2}\z/)`: one or two of the four atoms.
fn valid_trim_mode_string(mode: &str) -> bool {
    let mut rest = mode;
    let mut atoms = 0;
    while !rest.is_empty() {
        let atom = ["%", "-", ">", "<>"].iter().find(|a| rest.starts_with(**a));
        match atom {
            Some(a) => rest = &rest[a.len()..],
            None => return false,
        }
        atoms += 1;
    }
    (1..=2).contains(&atoms)
}

/// What a scanner hands the compiler: MRI's scanners yield plain strings (text
/// runs and tag spellings alike — the compiler tells them apart by equality),
/// the `:cr` symbol, and `PercentLine` objects.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    Str(String),
    Cr,
    PercentLine(String),
}

const ERB_STAG: [&str; 3] = ["<%=", "<%#", "<%"];

/// One alternative of a scanner regex, in the order MRI's alternation lists
/// them. Each is tried at every position from left to right, so the earliest
/// position wins and, at a tie, the first alternative listed.
#[derive(Clone, Copy)]
enum Alt {
    /// A literal spelling.
    Lit(&'static str),
    /// `^[ \t]*<%-`: blanks from a line start up to a `<%-`.
    LineStartDashTag,
    /// `%>\r?\n` or `-%>\r?\n`: a closing tag glued to its line break.
    TagNewline(&'static str),
    /// `\n`.
    Newline,
}

/// The `(.*?)(alt1|alt2|…|\z)` step every MRI scanner regex is built from:
/// the text before the earliest alternative, the alternative's spelling, and
/// the position after it. At end of input the `\z` alternative matches empty.
fn scan_step(src: &str, from: usize, alts: &[Alt]) -> (String, String, usize) {
    let bytes = src.as_bytes();
    let mut pos = from;
    while pos <= bytes.len() {
        if let Some(len) = alts.iter().find_map(|alt| match_alt(src, pos, *alt)) {
            let text = src[from..pos].to_string();
            let elem = src[pos..pos + len].to_string();
            return (text, elem, pos + len);
        }
        pos += 1;
        while pos < bytes.len() && !src.is_char_boundary(pos) {
            pos += 1;
        }
    }
    (src[from..].to_string(), String::new(), bytes.len())
}

fn match_alt(src: &str, pos: usize, alt: Alt) -> Option<usize> {
    let rest = &src[pos..];
    match alt {
        Alt::Lit(lit) => rest.starts_with(lit).then_some(lit.len()),
        Alt::Newline => rest.starts_with('\n').then_some(1),
        Alt::TagNewline(tag) => {
            let after = rest.strip_prefix(tag)?;
            if after.starts_with("\r\n") {
                Some(tag.len() + 2)
            } else if after.starts_with('\n') {
                Some(tag.len() + 1)
            } else {
                None
            }
        }
        Alt::LineStartDashTag => {
            let at_line_start = pos == 0 || src.as_bytes()[pos - 1] == b'\n';
            if !at_line_start {
                return None;
            }
            let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
            rest[blanks..].starts_with("<%-").then_some(blanks + 3)
        }
    }
}

/// Repeated `line.scan(reg)` over `src`, feeding each match's two groups (text,
/// then the alternative) to `f` — empty groups included, as MRI's block sees
/// them and skips them itself.
fn scan_all(src: &str, alts: &[Alt], mut f: impl FnMut(String)) {
    let mut pos = 0;
    loop {
        let (text, elem, next) = scan_step(src, pos, alts);
        f(text);
        f(elem);
        if next >= src.len() {
            break;
        }
        pos = next;
    }
}

/// `ERB::Compiler`: holds the scanner's `stag` (which tag is open), the pending
/// literal `content`, and the `Buffer` being written.
struct Compiler {
    spec: TrimSpec,
    stag: Option<String>,
    content: String,
    buffer: Buffer,
}

/// `ERB::Compiler::Buffer` with `ERB#set_eoutvar`'s commands: `_erbout = +''`
/// first, `_erbout` last, and `_erbout.<<` for both puts and inserts.
struct Buffer {
    script: String,
    line: Option<Vec<String>>,
}

const EOUTVAR: &str = "_erbout";

impl Buffer {
    fn new(coding: Option<&str>, frozen: Option<&str>) -> Buffer {
        let mut script = String::new();
        if let Some(enc) = coding {
            script.push_str(&format!("#coding:{enc}\n"));
        }
        if let Some(frozen) = frozen {
            script.push_str(&format!("#frozen-string-literal:{frozen}\n"));
        }
        Buffer {
            script,
            line: Some(vec![format!("{EOUTVAR} = +''")]),
        }
    }

    fn push(&mut self, cmd: String) {
        if let Some(line) = &mut self.line {
            line.push(cmd);
        }
    }

    fn cr(&mut self) {
        if let Some(line) = &mut self.line {
            self.script.push_str(&line.join("; "));
            line.clear();
            self.script.push('\n');
        }
    }

    fn close(&mut self) {
        let Some(mut line) = self.line.take() else {
            return;
        };
        line.push(EOUTVAR.to_string());
        self.script.push_str(&line.join("; "));
    }
}

impl Compiler {
    fn add_put_cmd(&mut self) {
        let newlines = self.content.matches('\n').count();
        let cmd = format!(
            "{EOUTVAR}.<< {}.freeze{}",
            dump(&self.content),
            "\n".repeat(newlines)
        );
        self.buffer.push(cmd);
    }

    fn add_insert_cmd(&mut self) {
        let cmd = format!("{EOUTVAR}.<<(({}).to_s)", self.content);
        self.buffer.push(cmd);
    }

    fn flush_content(&mut self) {
        if !self.content.is_empty() {
            self.add_put_cmd();
        }
        self.content.clear();
    }

    /// `compile_stag`: a token seen while no tag is open.
    fn compile_stag(&mut self, tok: Token) {
        match tok {
            Token::PercentLine(code) => {
                self.flush_content();
                self.buffer.push(code);
                self.buffer.cr();
            }
            Token::Cr => self.buffer.cr(),
            Token::Str(s) => match s.as_str() {
                "<%" | "<%=" | "<%#" => {
                    self.flush_content();
                    self.stag = Some(s);
                }
                "\n" => {
                    self.content.push('\n');
                    self.add_put_cmd();
                    self.content.clear();
                }
                "<%%" => self.content.push_str("<%"),
                _ => self.content.push_str(&s),
            },
        }
    }

    /// `compile_etag`: a token seen inside an open tag.
    fn compile_etag(&mut self, tok: Token) {
        match tok {
            Token::Str(s) if s == "%>" => {
                self.compile_content();
                self.stag = None;
                self.content.clear();
            }
            Token::Str(s) if s == "%%>" => self.content.push_str("%>"),
            Token::Str(s) => self.content.push_str(&s),
            // MRI's `case etag` has no `:cr`/`PercentLine` arm here: `else`
            // appends the token, and a Symbol or PercentLine appended to a
            // String would raise. The scanners never yield either while a tag
            // is open, so this arm is unreachable in practice.
            Token::Cr | Token::PercentLine(_) => {}
        }
    }

    /// `compile_content`: emit the open tag's body as its kind requires.
    fn compile_content(&mut self) {
        match self.stag.as_deref() {
            Some("<%") => {
                if self.content.ends_with('\n') {
                    self.content.pop();
                    let code = std::mem::take(&mut self.content);
                    self.buffer.push(code);
                    self.buffer.cr();
                } else {
                    let code = std::mem::take(&mut self.content);
                    self.buffer.push(code);
                }
            }
            Some("<%=") => self.add_insert_cmd(),
            Some("<%#") => {
                let newlines = self.content.matches('\n').count();
                self.buffer.push("\n".repeat(newlines));
            }
            _ => {}
        }
    }

    fn token(&mut self, tok: Token) {
        if matches!(&tok, Token::Str(s) if s.is_empty()) {
            return;
        }
        if self.stag.is_none() {
            self.compile_stag(tok);
        } else {
            self.compile_etag(tok);
        }
    }

    // ---- the scanners -------------------------------------------------------

    /// `Scanner.make_scanner`: the registered `SimpleScanner` for the plain
    /// mode, `ExplicitScanner` for `-`, and `TrimScanner` for everything else
    /// (every `%` mode included).
    fn scan(&mut self, src: &str) {
        match (self.spec.trim, self.spec.percent) {
            (Trim::None, false) => self.simple_scan(src),
            (Trim::Dash, false) => self.explicit_scan(src),
            _ => self.trim_scan(src),
        }
    }

    /// `SimpleScanner#scan`: `(.*?)(<%[%=#]?|\z)` while no tag is open,
    /// `(.*?)(%%?>|\z)` inside one.
    fn simple_scan(&mut self, src: &str) {
        const STAGS: [Alt; 4] = [
            Alt::Lit("<%%"),
            Alt::Lit("<%="),
            Alt::Lit("<%#"),
            Alt::Lit("<%"),
        ];
        const ETAGS: [Alt; 2] = [Alt::Lit("%%>"), Alt::Lit("%>")];
        let mut pos = 0;
        while pos < src.len() {
            let alts: &[Alt] = if self.stag.is_none() { &STAGS } else { &ETAGS };
            let (text, elem, next) = scan_step(src, pos, alts);
            self.token(Token::Str(text));
            self.token(Token::Str(elem));
            pos = next;
        }
    }

    /// `ExplicitScanner#scan` (the `-` mode): `(.*?)(^[ \t]*<%-|<%-|stags|\z)`
    /// while no tag is open, `(.*?)(-%>|etags|\z)` inside one. A `-%>` also
    /// swallows the line break (or the end of input) that follows it.
    fn explicit_scan(&mut self, src: &str) {
        const STAGS: [Alt; 6] = [
            Alt::LineStartDashTag,
            Alt::Lit("<%-"),
            Alt::Lit("<%%"),
            Alt::Lit("<%="),
            Alt::Lit("<%#"),
            Alt::Lit("<%"),
        ];
        const ETAGS: [Alt; 3] = [Alt::Lit("-%>"), Alt::Lit("%%>"), Alt::Lit("%>")];
        let mut pos = 0;
        while pos < src.len() {
            let alts: &[Alt] = if self.stag.is_none() { &STAGS } else { &ETAGS };
            let (text, elem, next) = scan_step(src, pos, alts);
            pos = next;
            self.token(Token::Str(text));
            if elem.ends_with("<%-") {
                self.token(Token::Str("<%".to_string()));
            } else if elem == "-%>" {
                self.token(Token::Str("%>".to_string()));
                let rest = &src[pos..];
                if rest.starts_with("\r\n") {
                    pos += 2;
                    self.token(Token::Cr);
                } else if rest.starts_with('\n') {
                    pos += 1;
                    self.token(Token::Cr);
                } else if rest.is_empty() {
                    self.token(Token::Cr);
                }
            } else {
                self.token(Token::Str(elem));
            }
        }
    }

    /// `TrimScanner#scan`: in a `%` mode each line is handed to `percent_line`,
    /// otherwise the whole source goes through the mode's line scanner.
    fn trim_scan(&mut self, src: &str) {
        if self.spec.percent {
            for line in each_line(src) {
                self.percent_line(line);
            }
        } else {
            self.scan_line(src);
        }
    }

    /// `TrimScanner#percent_line`: a line beginning `%` is Ruby code, `%%` is a
    /// literal `%`, and a line inside an open tag is never a percent line.
    fn percent_line(&mut self, line: &str) {
        if self.stag.is_some() || !line.starts_with('%') {
            return self.scan_line(line);
        }
        let line = &line[1..];
        if line.starts_with('%') {
            self.scan_line(line);
        } else {
            self.token(Token::PercentLine(chomp(line).to_string()));
        }
    }

    /// The `@scan_line` the trim mode selected.
    fn scan_line(&mut self, line: &str) {
        match self.spec.trim {
            Trim::Gt => self.trim_line1(line),
            Trim::LtGt => self.trim_line2(line),
            Trim::Dash => self.explicit_trim_line(line),
            Trim::None => self.plain_line(line),
        }
    }

    /// `(.*?)(%>\r?\n|stags|etags|\n|\z)` — the `>` and `<>` scanners' regex.
    const TRIM_ALTS: [Alt; 8] = [
        Alt::TagNewline("%>"),
        Alt::Lit("<%%"),
        Alt::Lit("<%="),
        Alt::Lit("<%#"),
        Alt::Lit("<%"),
        Alt::Lit("%%>"),
        Alt::Lit("%>"),
        Alt::Newline,
    ];

    /// `TrimScanner#scan_line`: `(.*?)(stags|etags|\n|\z)`.
    fn plain_line(&mut self, line: &str) {
        const ALTS: [Alt; 7] = [
            Alt::Lit("<%%"),
            Alt::Lit("<%="),
            Alt::Lit("<%#"),
            Alt::Lit("<%"),
            Alt::Lit("%%>"),
            Alt::Lit("%>"),
            Alt::Newline,
        ];
        scan_all(line, &ALTS, |tok| self.token(Token::Str(tok)));
    }

    /// `TrimScanner#trim_line1` (`>`): `%>` glued to a newline closes the tag
    /// and drops the newline.
    fn trim_line1(&mut self, line: &str) {
        scan_all(line, &Self::TRIM_ALTS, |tok| {
            if tok == "%>\n" || tok == "%>\r\n" {
                self.token(Token::Str("%>".to_string()));
                self.token(Token::Cr);
            } else {
                self.token(Token::Str(tok));
            }
        });
    }

    /// `TrimScanner#trim_line2` (`<>`): the newline after `%>` is dropped only
    /// when the line's first token was an ERB start tag.
    fn trim_line2(&mut self, line: &str) {
        let mut head: Option<String> = None;
        scan_all(line, &Self::TRIM_ALTS, |tok| {
            if tok.is_empty() {
                return;
            }
            if head.is_none() {
                head = Some(tok.clone());
            }
            if tok == "%>\n" || tok == "%>\r\n" {
                self.token(Token::Str("%>".to_string()));
                let starts_with_tag = head.as_deref().is_some_and(|h| ERB_STAG.contains(&h));
                if starts_with_tag {
                    self.token(Token::Cr);
                } else {
                    self.token(Token::Str("\n".to_string()));
                }
                head = None;
            } else {
                let is_newline = tok == "\n";
                self.token(Token::Str(tok));
                if is_newline {
                    head = None;
                }
            }
        });
    }

    /// `TrimScanner#explicit_trim_line` (`-` with `%`):
    /// `(.*?)(^[ \t]*<%-|<%-|-%>\r?\n|-%>|stags|etags|\z)`.
    fn explicit_trim_line(&mut self, line: &str) {
        const ALTS: [Alt; 10] = [
            Alt::LineStartDashTag,
            Alt::Lit("<%-"),
            Alt::TagNewline("-%>"),
            Alt::Lit("-%>"),
            Alt::Lit("<%%"),
            Alt::Lit("<%="),
            Alt::Lit("<%#"),
            Alt::Lit("<%"),
            Alt::Lit("%%>"),
            Alt::Lit("%>"),
        ];
        scan_all(line, &ALTS, |tok| {
            if tok.is_empty() {
                return;
            }
            let open = self.stag.is_some();
            if !open && tok.ends_with("<%-") {
                self.token(Token::Str("<%".to_string()));
            } else if open && (tok == "-%>\n" || tok == "-%>\r\n") {
                self.token(Token::Str("%>".to_string()));
                self.token(Token::Cr);
            } else if open && tok == "-%>" {
                self.token(Token::Str("%>".to_string()));
            } else {
                self.token(Token::Str(tok));
            }
        });
    }
}

/// `String#each_line` with the default separator: every piece keeps its `\n`.
fn each_line(src: &str) -> impl Iterator<Item = &str> {
    src.split_inclusive('\n')
}

/// `String#chomp`: drop one trailing `\r\n`, `\n` or `\r`.
fn chomp(s: &str) -> &str {
    s.strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .or_else(|| s.strip_suffix('\r'))
        .unwrap_or(s)
}

/// `String#dump` on the template text — the double-quoted literal the put
/// command carries. ASCII escapes are MRI's (`\#` ahead of `{`, `$` and `@`
/// keeps template text out of interpolation); a non-ASCII character is kept
/// as itself rather than as MRI's `\xNN` byte escapes (see the module doc).
fn dump(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            '\x08' => out.push_str("\\b"),
            '\x07' => out.push_str("\\a"),
            '\x1b' => out.push_str("\\e"),
            '#' => {
                if matches!(chars.get(i + 1), Some('$' | '@' | '{')) {
                    out.push('\\');
                }
                out.push('#');
            }
            ' '..='~' => out.push(c),
            c if (c as u32) < 0x80 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `ERB::Compiler#detect_magic_comment`: leading `<%# … %>` tags (and `%# …`
/// lines in a `%` mode) are read for `coding:` and `frozen_string_literal:`
/// directives, optionally inside an Emacs `-*- … -*-` stanza. Answers the
/// coding name as written and the frozen value.
fn detect_magic_comment(src: &str, percent: bool) -> (Option<String>, Option<String>) {
    let mut coding = None;
    let mut frozen = None;
    let mut pos = 0;
    loop {
        let rest = &src[pos..];
        let (comment, len) = if let Some(body) = rest.strip_prefix("<%#") {
            // `(.*)%>` is greedy within the line: up to the LAST `%>` before a newline.
            let line_end = body.find('\n').unwrap_or(body.len());
            let Some(close) = body[..line_end].rfind("%>") else {
                break;
            };
            (&body[..close], 3 + close + 2)
        } else if let Some(body) = percent.then(|| rest.strip_prefix("%#")).flatten() {
            let Some(nl) = body.find('\n') else { break };
            (&body[..nl], 2 + nl + 1)
        } else {
            break;
        };
        pos += len;
        let comment = emacs_stanza(comment).unwrap_or(comment);
        if let Some(name) = directive(comment, "coding") {
            coding = Some(name);
        } else if let Some(value) = frozen_directive(comment) {
            frozen = Some(value);
        }
    }
    (coding, frozen)
}

/// `comment[/-\*-\s*([^\s].*?)\s*-\*-$/]`: the body of an Emacs mode line,
/// which must close the comment.
fn emacs_stanza(comment: &str) -> Option<&str> {
    let inner = comment.strip_suffix("-*-")?;
    let start = inner.find("-*-")? + 3;
    let body = inner[start..].trim();
    (!body.is_empty()).then_some(body)
}

/// `%r"coding\s*[=:]\s*([[:alnum:]\-_]+)"`, with MRI's `-(mac|dos|unix)` suffix
/// strip applied to the captured name.
fn directive(comment: &str, key: &str) -> Option<String> {
    let at = comment.find(key)? + key.len();
    let rest = comment[at..].trim_start();
    let rest = rest.strip_prefix(['=', ':'])?.trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let stripped = ["-mac", "-dos", "-unix"].iter().find_map(|suffix| {
        lower
            .find(suffix)
            .map(|i| format!("{}{}", &name[..i], &name[i + suffix.len()..]))
    });
    Some(stripped.unwrap_or(name))
}

/// `%r"frozen[-_]string[-_]literal\s*:\s*([[:alnum:]]+)"`.
fn frozen_directive(comment: &str) -> Option<String> {
    let at = [
        "frozen_string_literal",
        "frozen-string-literal",
        "frozen_string-literal",
        "frozen-string_literal",
    ]
    .iter()
    .find_map(|k| comment.find(k).map(|i| i + k.len()))?;
    let rest = comment[at..].trim_start().strip_prefix(':')?.trim_start();
    let value: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (!value.is_empty()).then_some(value)
}

/// `ERB::Compiler#compile` with `ERB#set_eoutvar`'s commands: the Ruby source
/// `ERB#src` answers. `resolve_encoding` is `Encoding.find` for a `coding:`
/// magic comment — `Err` is the `ArgumentError` it raises for an unknown name.
/// Without a magic comment the header names the template's encoding, UTF-8.
pub fn compile(
    template: &str,
    spec: TrimSpec,
    resolve_encoding: &dyn Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let (coding, frozen) = detect_magic_comment(template, spec.percent);
    let coding = match coding {
        Some(name) => resolve_encoding(&name)?,
        None => "UTF-8".to_string(),
    };
    let mut c = Compiler {
        spec,
        stag: None,
        content: String::new(),
        buffer: Buffer::new(Some(&coding), frozen.as_deref()),
    };
    c.scan(template);
    if !c.content.is_empty() {
        c.add_put_cmd();
    }
    c.buffer.close();
    Ok(c.buffer.script)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(template: &str, mode: TrimArg) -> String {
        let (spec, _) = prepare_trim_mode(&mode);
        compile(template, spec, &|n| Ok(n.to_uppercase())).unwrap()
    }

    const T1: &str = "a\n<% x = 1 %>\nb <%= x %> c\n<%# note %>\n<%% lit %%>\nend";
    const T2: &str = "<% [1,2].each do |i| %>\n  item <%= i %>\n<% end %>\ntail\n";
    const T3: &str = "% x = 5\nplain\n%% not code\nv=<%= x %>\n% if x > 1\nbig\n% end\nmid % not\n";
    const T4: &str = "  <%- y = 2 -%>\nfoo  <%- y -%>  bar\n<%= y -%>\nlast <%- 1 -%>";
    const T7: &str = "<%\n  z = 3\n%>\nv<%= z %>\n<%#\n multi\n%>\nq";

    // Every expectation below is `ERB.new(t, trim_mode: m).src` from ruby 4.0.7 / erb 6.0.7.
    #[test]
    fn plain_mode_is_the_simple_scanner() {
        assert_eq!(
            src(T1, TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"a\\n\".freeze\n;  x = 1 ; _erbout.<< \"\\nb \".freeze\n; _erbout.<<(( x ).to_s); _erbout.<< \" c\\n\".freeze\n; ; _erbout.<< \"\\n\".freeze\n; _erbout.<< \"<% lit %%>\\nend\".freeze\n; _erbout"
        );
        assert_eq!(
            src(T7, TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; \n  z = 3\n_erbout.<< \"\\nv\".freeze\n; _erbout.<<(( z ).to_s); _erbout.<< \"\\n\".freeze\n; \n\n; _erbout.<< \"\\nq\".freeze\n; _erbout"
        );
        assert_eq!(
            src("", TrimArg::Str("<>".into())),
            "#coding:UTF-8\n_erbout = +''; _erbout"
        );
        assert_eq!(
            src("unterminated <% x", TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"unterminated \".freeze; _erbout.<< \" x\".freeze; _erbout"
        );
        assert_eq!(
            src("a<% 1 \n %>b", TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"a\".freeze;  1 \n ; _erbout.<< \"b\".freeze; _erbout"
        );
        assert_eq!(
            src("a #{1+1} b", TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"a \\#{1+1} b\".freeze; _erbout"
        );
        assert_eq!(
            src("x <%= '%%>' %> y", TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"x \".freeze; _erbout.<<(( '%>' ).to_s); _erbout.<< \" y\".freeze; _erbout"
        );
    }

    #[test]
    fn angle_modes() {
        assert_eq!(
            src(T2, TrimArg::Str("<>".into())),
            "#coding:UTF-8\n_erbout = +'';  [1,2].each do |i| \n_erbout.<< \"  item \".freeze; _erbout.<<(( i ).to_s); _erbout.<< \"\\n\".freeze\n;  end \n_erbout.<< \"tail\\n\".freeze\n; _erbout"
        );
        assert_eq!(
            src("x<%= 1 %>\n<% 2 %>\n", TrimArg::Int(1)),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"x\".freeze; _erbout.<<(( 1 ).to_s)\n 2 \n_erbout"
        );
    }

    #[test]
    fn percent_mode() {
        assert_eq!(
            src(T3, TrimArg::Str("%".into())),
            "#coding:UTF-8\n_erbout = +'';  x = 5\n_erbout.<< \"plain\\n\".freeze\n; _erbout.<< \"% not code\\n\".freeze\n; _erbout.<< \"v=\".freeze; _erbout.<<(( x ).to_s); _erbout.<< \"\\n\".freeze\n;  if x > 1\n_erbout.<< \"big\\n\".freeze\n;  end\n_erbout.<< \"mid % not\\n\".freeze\n; _erbout"
        );
    }

    #[test]
    fn dash_mode_is_the_explicit_scanner() {
        assert_eq!(
            src(T4, TrimArg::Str("-".into())),
            "#coding:UTF-8\n_erbout = +'';  y = 2 \n_erbout.<< \"foo  \".freeze;  y ; _erbout.<< \"  bar\\n\".freeze\n; _erbout.<<(( y ).to_s)\n_erbout.<< \"last \".freeze;  1 \n_erbout"
        );
        assert_eq!(
            src("<%- 1 -%>", TrimArg::Str("-".into())),
            "#coding:UTF-8\n_erbout = +'';  1 \n_erbout"
        );
        assert_eq!(
            src("a <%- 1 -%> b\n", TrimArg::Str("-".into())),
            "#coding:UTF-8\n_erbout = +''; _erbout.<< \"a \".freeze;  1 ; _erbout.<< \" b\\n\".freeze\n; _erbout"
        );
    }

    #[test]
    fn trim_mode_argument() {
        let spec = |p, t| TrimSpec {
            percent: p,
            trim: t,
        };
        assert_eq!(
            prepare_trim_mode(&TrimArg::Nil),
            (spec(false, Trim::None), None)
        );
        assert_eq!(
            prepare_trim_mode(&TrimArg::Int(0)),
            (spec(false, Trim::None), None)
        );
        assert_eq!(
            prepare_trim_mode(&TrimArg::Int(1)),
            (spec(false, Trim::Gt), None)
        );
        assert_eq!(
            prepare_trim_mode(&TrimArg::Int(2)),
            (spec(false, Trim::LtGt), None)
        );
        assert_eq!(
            prepare_trim_mode(&TrimArg::Str("%<>".into())),
            (spec(true, Trim::LtGt), None)
        );
        assert_eq!(
            prepare_trim_mode(&TrimArg::Str("->".into())),
            (spec(false, Trim::Dash), None)
        );
        assert_eq!(
            prepare_trim_mode(&TrimArg::Str("%>".into())),
            (spec(true, Trim::Gt), None)
        );
        let (s, w) = prepare_trim_mode(&TrimArg::Str("x".into()));
        assert_eq!(s, spec(false, Trim::None));
        assert_eq!(
            w.unwrap(),
            "Invalid ERB trim mode: \"x\" (trim_mode: nil, 0, 1, 2, or String composed of '%' and/or '-', '>', '<>')"
        );
        let (s, w) = prepare_trim_mode(&TrimArg::Int(3));
        assert_eq!(s, spec(false, Trim::None));
        assert!(w.unwrap().starts_with("Invalid ERB trim mode: 3 ("));
    }

    #[test]
    fn magic_comments() {
        assert_eq!(
            src("<%# -*- coding: us-ascii -*- %> trailing %>x", TrimArg::Nil),
            "#coding:US-ASCII\n_erbout = +''; ; _erbout.<< \" trailing %>x\".freeze; _erbout"
        );
        assert_eq!(
            src("%# coding: us-ascii\nx", TrimArg::Str("%".into())),
            "#coding:US-ASCII\n_erbout = +''; # coding: us-ascii\n_erbout.<< \"x\".freeze; _erbout"
        );
        assert_eq!(
            src("<%# coding: utf-8-mac %>\nx", TrimArg::Nil),
            "#coding:UTF-8\n_erbout = +''; ; _erbout.<< \"\\nx\".freeze\n; _erbout"
        );
        assert_eq!(
            src("<%# -*- coding: us-ascii -*- %>x", TrimArg::Nil),
            "#coding:US-ASCII\n_erbout = +''; ; _erbout.<< \"x\".freeze; _erbout"
        );
        assert_eq!(
            src("<%# frozen_string_literal: true %>\nx", TrimArg::Nil),
            "#coding:UTF-8\n#frozen-string-literal:true\n_erbout = +''; ; _erbout.<< \"\\nx\".freeze\n; _erbout"
        );
    }
}
