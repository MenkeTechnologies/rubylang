//! Interactive REPL for `ruby` — utop-style line editor backed by `reedline`,
//! ported from the strykelang REPL onto the Ruby host.
//!
//! Layout per turn:
//!
//! ```text
//! ─( HH:MM:SS )──< command N >─────────────────────────────{ ruby 0.1.0 }─
//! ruby❯ <buffer>
//!       abs   each   inspect   length   map   push   times   …
//! ```
//!
//! * Top "modeline" is rendered as part of `Prompt::render_prompt_left` so it
//!   repaints with the buffer (no scroll-off, no flicker).
//! * Tab pops a `ColumnarMenu` of suggestions sourced from the Ruby keyword set
//!   plus the LSP builtin corpus (`lsp::corpus`) plus the live host's method /
//!   class / constant / global names — so a `def` or `class` made on a prior
//!   prompt completes on the next one.
//! * History is `~/.rubylang/history` via `FileBackedHistory`.
//! * Each accepted line is compiled and run on a *persistent* host, so `def`s
//!   and variables carry across prompts. The value is echoed with `inspect`
//!   (the `=>` form), like `irb`.

use std::borrow::Cow;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use nu_ansi_term::{Color as NuColor, Style};
use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    ColumnarMenu, Completer, EditMode, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    Keybindings, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
    PromptHistorySearchStatus, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion, Vi,
};

use crate::{banner, host, lsp};

const RUBY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Ruby reserved words (mirrors `lexer::KEYWORDS`, kept local so the REPL does
/// not depend on the lexer's private const).
const KEYWORDS: &[&str] = &[
    "def",
    "end",
    "if",
    "elsif",
    "else",
    "unless",
    "while",
    "until",
    "for",
    "in",
    "do",
    "return",
    "break",
    "next",
    "yield",
    "then",
    "case",
    "when",
    "nil",
    "true",
    "false",
    "and",
    "or",
    "not",
    "class",
    "module",
    "begin",
    "rescue",
    "ensure",
    "self",
    "super",
    "puts",
    "print",
    "p",
    "raise",
    "require",
    "attr_accessor",
    "attr_reader",
    "attr_writer",
    "lambda",
    "proc",
];

fn rubylang_dir() -> std::path::PathBuf {
    let dir = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".rubylang"))
        .unwrap_or_else(|| std::path::PathBuf::from(".rubylang"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn history_path() -> std::path::PathBuf {
    rubylang_dir().join("history")
}

fn config_path() -> std::path::PathBuf {
    rubylang_dir().join("config.toml")
}

/// Contents of the auto-seeded `~/.rubylang/config.toml`. Every setting is
/// commented out so the seeded file documents the schema without changing
/// behavior — uncomment + edit a line to override the in-code default.
const DEFAULT_CONFIG_TOML: &str = r#"# rubylang runtime config — auto-generated on first launch.
# Lines starting with `#` are comments. Uncomment + edit a line to
# override the in-code default. Delete this file and rubylang will
# regenerate it on the next run.

[repl]
# Edit mode for the interactive REPL. Defaults to emacs.
#
#   "emacs" — Ctrl-A/Ctrl-E/Ctrl-K/etc., readline-style (default)
#   "vi"    — modal editing; Esc → normal mode, i/a → insert,
#             h/j/k/l navigation, dd/cc/yy/x, /-search, etc.
#
# Tab + Shift+Tab cycle the completion menu in either mode.
# Override per-session with `RUBYLANG_REPL_MODE=vi ruby`.
# mode = "emacs"
"#;

/// First-run seed: write `~/.rubylang/config.toml` if it does not exist. Safe
/// to call on every launch — no-op when the file is already there (and silent
/// if the home directory is read-only). Honors `RUBYLANG_NO_CONFIG=1` for
/// CI / sandbox environments that should not touch the user's home dir.
pub fn ensure_default_config_seeded() {
    if std::env::var_os("RUBYLANG_NO_CONFIG").is_some() {
        return;
    }
    let path = config_path();
    if path.exists() {
        return;
    }
    let _ = std::fs::write(&path, DEFAULT_CONFIG_TOML);
}

/// REPL edit-mode selector. `Emacs` is the default; `Vi` enables reedline's
/// two-mode insert/normal keybinding set with the standard `Esc` toggle.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ReplMode {
    Emacs,
    Vi,
}

/// Resolve the REPL edit mode in this precedence:
/// 1. `RUBYLANG_REPL_MODE=emacs|vi` env var (overrides everything).
/// 2. `~/.rubylang/config.toml` `[repl] mode = "vi"`.
/// 3. Default `Emacs`.
fn resolve_repl_mode() -> ReplMode {
    if let Some(env) = std::env::var_os("RUBYLANG_REPL_MODE") {
        let s = env.to_string_lossy().to_ascii_lowercase();
        if s == "vi" || s == "vim" {
            return ReplMode::Vi;
        }
        if s == "emacs" {
            return ReplMode::Emacs;
        }
    }
    let raw = match std::fs::read_to_string(config_path()) {
        Ok(s) => s,
        Err(_) => return ReplMode::Emacs,
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return ReplMode::Emacs,
    };
    let mode = parsed
        .get("repl")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("emacs");
    match mode.to_ascii_lowercase().as_str() {
        "vi" | "vim" => ReplMode::Vi,
        _ => ReplMode::Emacs,
    }
}

/// Apply the completion-menu Tab / Shift+Tab bindings to a keybinding set —
/// shared so the bindings live on the emacs map AND the vi insert map.
fn install_menu_bindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings.add_binding(
        KeyModifiers::SHIFT,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
}

fn build_static_completions() -> Vec<String> {
    let mut v: Vec<String> = KEYWORDS.iter().map(|s| (*s).to_string()).collect();
    v.extend(
        lsp::corpus()
            .iter()
            .map(|(name, _, _, _)| (*name).to_string()),
    );
    v.sort();
    v.dedup();
    v
}

/// Byte index `start` and the incomplete word before cursor (for prefix
/// matching). Word boundaries include whitespace and punctuation; if the tail
/// contains a Ruby sigil (`$`, `@`), the start snaps to it so variables
/// complete as `$name` / `@name` / `@@name`.
fn completion_word_start(line: &str, pos: usize) -> (usize, &str) {
    let pos = pos.min(line.len());
    let before = line.get(..pos).unwrap_or("");
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| {
            c.is_whitespace()
                || matches!(
                    *c,
                    '(' | ')' | ',' | ';' | '[' | ']' | '{' | '}' | '|' | '=' | '&' | '+' | '.'
                )
        })
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let mut word_start = start;
    let tail = line.get(word_start..pos).unwrap_or("");
    if let Some(rel) = tail.find(['$', '@']) {
        word_start += rel;
    }
    (word_start, line.get(word_start..pos).unwrap_or(""))
}

/// Whether the cursor sits just after a `receiver.` — i.e. a method-call
/// context where only method names (not keywords / top-level names) should be
/// offered. Returns the byte start of the partial method name after the dot.
fn method_context(line: &str, pos: usize) -> Option<usize> {
    let pos = pos.min(line.len());
    let before = line.get(..pos)?;
    // The partial method name is the run of ident chars right before the cursor.
    let name_start = before
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '?' || *c == '!'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    // A `.` immediately precedes the name, and a receiver precedes that dot.
    let pre = line.get(..name_start)?;
    let recv_str = pre.strip_suffix('.')?;
    // A `.` after `)` / `]` / `"` is a method call on an expression result.
    let last = recv_str.chars().next_back()?;
    if last == ')' || last == ']' || last == '"' {
        return Some(name_start);
    }
    // Otherwise the receiver is a bare token: the run of ident chars before the
    // dot. It is a method call unless that token is a pure numeric literal —
    // `3.14` is a Float, not `3` dot-`14`.
    let recv_start = recv_str
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let recv_tok = recv_str.get(recv_start..).unwrap_or("");
    if recv_tok.is_empty() || recv_tok.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        Some(name_start)
    }
}

struct RubyCompleter {
    static_words: Vec<String>,
    method_words: Vec<String>,
    dynamic: Arc<Mutex<Vec<String>>>,
}

impl RubyCompleter {
    fn suggestions_from<'a>(
        words: impl Iterator<Item = &'a String>,
        prefix: &str,
        span: Span,
    ) -> Vec<Suggestion> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out: Vec<Suggestion> = Vec::new();
        for w in words {
            if !w.starts_with(prefix) || !seen.insert(w.as_str()) {
                continue;
            }
            out.push(Suggestion {
                value: w.clone(),
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: false,
                display_override: None,
                match_indices: None,
            });
        }
        out.sort_by(|a, b| a.value.cmp(&b.value));
        out
    }
}

impl Completer for RubyCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        // 1. `receiver.method` — offer method names only.
        if let Some(start) = method_context(line, pos) {
            let prefix = line.get(start..pos).unwrap_or("");
            let span = Span::new(start, pos);
            return Self::suggestions_from(self.method_words.iter(), prefix, span);
        }
        // 2. bare word / sigil-prefixed name completion.
        let (start, prefix) = completion_word_start(line, pos);
        let span = Span::new(start, pos);
        let dyn_list = match self.dynamic.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        Self::suggestions_from(
            self.static_words.iter().chain(dyn_list.iter()),
            prefix,
            span,
        )
    }
}

struct RubyPrompt {
    cmd_count: Arc<Mutex<u64>>,
}

fn now_hms() -> String {
    // Local time via `libc::localtime_r` — no chrono / time crate. On failure
    // or an invalid epoch, falls back to UTC modulo math so the status bar
    // always shows something.
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok {
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    } else {
        let s = (secs as u64) % 86_400;
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

fn term_cols() -> usize {
    use std::os::unix::io::AsRawFd;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = std::io::stdout().as_raw_fd();
    let cols = if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(80)
    };
    cols.max(40)
}

fn render_status_bar(cmd_count: u64) -> String {
    let cols = term_cols();
    let dim = NuColor::DarkGray;
    let accent = NuColor::Cyan;
    let label = NuColor::LightYellow;

    let left = format!(" {} ", now_hms());
    let mid = format!(" command {} ", cmd_count);
    let right = format!(" ruby {} ", RUBY_VERSION);

    // `frame_chars` = display width of every literal frame char emitted below.
    let frame_chars = "─()──<>{}─".chars().count();
    let visible = left.chars().count() + mid.chars().count() + right.chars().count() + frame_chars;
    let dashes = cols.saturating_sub(visible);
    if dashes < 2 {
        return format!(
            "{lp}{l}{rp}{ml}{m}{mr}",
            lp = Style::new().fg(dim).paint("─("),
            l = Style::new().fg(accent).paint(left),
            rp = Style::new().fg(dim).paint(")"),
            ml = Style::new().fg(dim).paint("──<"),
            m = Style::new().fg(label).bold().paint(mid),
            mr = Style::new().fg(dim).paint(">"),
        );
    }
    let left_dash = dashes / 2;
    let right_dash = dashes - left_dash;
    let bar_l = "─".repeat(left_dash);
    let bar_r = "─".repeat(right_dash);

    format!(
        "{lp}{l}{rp}{ml}{m}{mr}{bar}{rl}{r}{rr}",
        lp = Style::new().fg(dim).paint("─("),
        l = Style::new().fg(accent).paint(left),
        rp = Style::new().fg(dim).paint(")"),
        ml = Style::new().fg(dim).paint("──<"),
        m = Style::new().fg(label).bold().paint(mid),
        mr = Style::new().fg(dim).paint(">"),
        bar = Style::new().fg(dim).paint(format!("{}{}", bar_l, bar_r)),
        rl = Style::new().fg(dim).paint("{"),
        r = Style::new().fg(NuColor::Magenta).paint(right),
        rr = Style::new().fg(dim).paint("}─"),
    )
}

impl Prompt for RubyPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let count = self.cmd_count.lock().map(|g| *g).unwrap_or(0);
        let bar = render_status_bar(count);
        let prompt = Style::new()
            .fg(NuColor::Cyan)
            .bold()
            .paint("ruby")
            .to_string();
        Cow::Owned(format!("{}\n{}", bar, prompt))
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Owned(
            Style::new()
                .fg(NuColor::LightCyan)
                .bold()
                .paint("❯ ")
                .to_string(),
        )
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(
            Style::new()
                .fg(NuColor::DarkGray)
                .paint("····❯ ")
                .to_string(),
        )
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }
}

/// Run the REPL until `exit` / `quit` / Ctrl-D.
pub fn run() {
    banner::print_banner();
    host::reset_host();
    ensure_default_config_seeded();

    println!();
    println!("\x1b[2m  type `exit` or Ctrl-D to leave the REPL — Tab for completion\x1b[0m");
    println!();

    let static_words = build_static_completions();
    let method_words: Vec<String> = {
        let mut v: Vec<String> = lsp::corpus()
            .iter()
            .map(|(n, _, _, _)| (*n).to_string())
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let dynamic = Arc::new(Mutex::new(host::with_host(|h| h.repl_completion_names())));
    let cmd_count = Arc::new(Mutex::new(0u64));

    let completer = RubyCompleter {
        static_words,
        method_words,
        dynamic: Arc::clone(&dynamic),
    };

    let menu = ColumnarMenu::default()
        .with_name("completion_menu")
        .with_columns(4)
        .with_column_padding(2);

    let edit_mode: Box<dyn EditMode> = match resolve_repl_mode() {
        ReplMode::Emacs => {
            let mut kb = default_emacs_keybindings();
            install_menu_bindings(&mut kb);
            Box::new(Emacs::new(kb))
        }
        ReplMode::Vi => {
            let mut insert_kb = default_vi_insert_keybindings();
            install_menu_bindings(&mut insert_kb);
            let normal_kb = default_vi_normal_keybindings();
            Box::new(Vi::new(insert_kb, normal_kb))
        }
    };

    let history = match FileBackedHistory::with_file(5_000, history_path()) {
        Ok(h) => Box::new(h) as Box<dyn reedline::History>,
        Err(e) => {
            eprintln!("repl: history unavailable: {}", e);
            Box::new(FileBackedHistory::new(5_000).unwrap_or_else(|_| {
                eprintln!("repl: cannot create in-memory history");
                process::exit(1);
            })) as Box<dyn reedline::History>
        }
    };

    let mut line_editor = Reedline::create()
        .with_completer(Box::new(completer))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_edit_mode(edit_mode)
        .with_history(history);

    let prompt = RubyPrompt {
        cmd_count: Arc::clone(&cmd_count),
    };

    loop {
        // Refresh the live completion set so `def`/`class`/`$g = …` from the
        // prior line completes on this one.
        if let Ok(mut g) = dynamic.lock() {
            *g = host::with_host(|h| h.repl_completion_names());
        }

        let sig = match line_editor.read_line(&prompt) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("repl: {}", e);
                break;
            }
        };

        match sig {
            Signal::Success(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(trimmed, "exit" | "quit") {
                    break;
                }
                if let Ok(mut g) = cmd_count.lock() {
                    *g += 1;
                }
                eval_line(trimmed);
            }
            Signal::CtrlC => continue,
            Signal::CtrlD => break,
            _ => break,
        }
    }
}

/// Compile+run a single line on the live host, echoing the result with
/// `inspect` (the `=>` form), like `irb`.
fn eval_line(line: &str) {
    let prog = match crate::compile(line) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", NuColor::Red.paint(format!("ruby: {e}")));
            return;
        }
    };
    // Rebase + merge this line's program onto the live host so its proc/begin
    // ids don't collide with earlier lines' already-loaded ids.
    let main = crate::load_merged(prog);
    match host::run_main(main) {
        Ok(v) => {
            let s = host::with_host(|h| h.inspect(&v));
            println!(
                "{} {}",
                NuColor::DarkGray.paint("=>"),
                NuColor::Green.paint(s)
            );
        }
        Err(e) => eprintln!("{}", NuColor::Red.paint(format!("ruby: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_word_at_cursor_includes_sigil() {
        let s = "puts @foo";
        let (st, pre) = completion_word_start(s, s.len());
        assert_eq!(st, 5);
        assert_eq!(pre, "@foo");
    }

    #[test]
    fn completion_start_after_space() {
        let s = "x = ";
        let (st, pre) = completion_word_start(s, s.len());
        assert_eq!(st, 4);
        assert_eq!(pre, "");
    }

    #[test]
    fn method_context_after_dot() {
        let s = "arr.ma";
        let start = method_context(s, s.len()).expect("method context");
        assert_eq!(&s[start..], "ma");
    }

    #[test]
    fn no_method_context_for_leading_dot_number() {
        // `3.14` is a float, not a method call — must not trigger method mode.
        assert!(method_context("3.14", 4).is_none());
    }

    #[test]
    fn static_completions_include_keywords_and_corpus() {
        let v = build_static_completions();
        assert!(v.iter().any(|w| w == "class"));
        assert!(v.iter().any(|w| w == "inspect"));
        assert!(v.iter().any(|w| w == "each"));
    }
}
