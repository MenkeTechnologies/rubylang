//! Command-line interface for the `ruby` binary.
//!
//! `ruby(1)` has an option grammar clap cannot model faithfully — `-e` is
//! repeatable and switches off the program-file slot (the first non-switch token
//! then becomes `ARGV[0]`), short switches bundle (`-ne`, `-wc`), value switches
//! glue their argument (`-Idir`, `-rlib`), and everything after the program file
//! (or after `--`) is `ARGV`, not more switches. So this is a hand-rolled parser
//! that mirrors MRI's `proc_options`, plus rubylang's own long options
//! (`--repl`/`--lsp`/`--dap`/`--build`/`--dump-*`/`--disasm`).

/// The parsed command line. Boolean rubylang extensions keep their old names so
/// `main.rs` dispatch is unchanged; the MRI-compat fields are new.
#[derive(Debug, Default)]
pub struct Cli {
    // ---- MRI program selection --------------------------------------------
    /// `-e SRC` snippets, in order (MRI joins repeated `-e` with newlines).
    pub eval: Vec<String>,
    /// `-r LIB` libraries to `require` before the program runs.
    pub requires: Vec<String>,
    /// `-I DIR` directories prepended to `$LOAD_PATH`.
    pub includes: Vec<String>,
    /// The program file to run (`None` with `-e`, or when reading stdin).
    pub file: Option<String>,
    /// Arguments passed to the program as `ARGV`.
    pub argv: Vec<String>,

    // ---- MRI switches ------------------------------------------------------
    /// `-c` — check syntax only, print `Syntax OK`, do not run.
    pub check_syntax: bool,
    /// `-w`/`-W` warning level (0 = silent, 1 = default `-w`, 2 = verbose).
    pub warn_level: u8,
    /// `-n` — wrap the program in `while gets; … end`.
    pub loop_n: bool,
    /// `-p` — like `-n` but print `$_` at the end of each iteration.
    pub loop_p: bool,
    /// `-a` — autosplit each line into `$F` (with `-n`/`-p`).
    pub autosplit: bool,
    /// `-l` — chomp the input record separator on each `gets`.
    pub chomp: bool,
    /// `-F PAT` — the `$_.split` pattern for `-a`.
    pub field_sep: Option<String>,
    /// `-S` — search `$PATH` for the program file.
    pub search_path: bool,
    /// `-d`/`--debug` — set `$DEBUG`.
    pub debug: bool,
    /// `-v` — print the version banner, then run any program.
    pub verbose_version: bool,

    // ---- exit-early informational -----------------------------------------
    /// `--version` (or bare `-v` with no program) — print version and exit.
    pub show_version: bool,
    /// `--help`/`-h`.
    pub show_help: bool,

    // ---- rubylang extensions (unchanged) ----------------------------------
    pub repl: bool,
    pub lsp: bool,
    pub dap: bool,
    pub build: bool,
    pub native: bool,
    pub dump_bytecode: bool,
    pub dump_tokens: bool,
    pub dump_ast: bool,
    pub disasm: bool,
}

/// Parse the real process arguments (skipping `argv[0]`).
pub fn parse() -> Result<Cli, String> {
    parse_args(std::env::args().skip(1).collect())
}

/// Parse an explicit argument vector (testable, no `argv[0]`).
pub fn parse_args(args: Vec<String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut it = args.into_iter().peekable();

    // Phase 1: switches, until the first non-switch token, `--`, `-`, or a token
    // consumed as the program file.
    while let Some(tok) = it.peek() {
        if tok == "--" {
            it.next();
            break;
        }
        if tok == "-" {
            // A bare `-` means read the program from stdin; it is the "file".
            it.next();
            cli.file = Some("-".to_string());
            break;
        }
        if let Some(long) = tok.strip_prefix("--") {
            let long = long.to_string();
            it.next();
            apply_long(&mut cli, &long)?;
            continue;
        }
        if let Some(short) = tok.strip_prefix('-') {
            if short.is_empty() {
                break;
            }
            let short = short.to_string();
            it.next();
            // A short cluster may consume the next token (for `-e`/`-I`/`-r`/`-F`
            // with a detached argument). Switch scanning continues even after
            // `-e` — MRI keeps parsing switches (more `-e`/`-r`/…) until the first
            // non-switch token, which then becomes ARGV.
            apply_short_cluster(&mut cli, &short, &mut it)?;
            continue;
        }
        // First non-switch token. With `-e` there is no program file, so it (and
        // the rest) is ARGV; otherwise it is the program file.
        if cli.eval.is_empty() {
            cli.file = Some(it.next().unwrap());
        }
        break;
    }

    // With `-e`, the program is the snippets and there is no file; everything
    // left (after `--` or the first non-switch token) is ARGV.
    if !cli.eval.is_empty() {
        cli.argv = it.collect();
        return Ok(cli);
    }
    if cli.file.is_none() {
        // `--` with a following file, or a program file after long switches.
        if let Some(next) = it.next() {
            cli.file = Some(next);
        }
    }
    cli.argv = it.collect();
    Ok(cli)
}

/// A `--long` option (rubylang extensions plus a few MRI long forms).
fn apply_long(cli: &mut Cli, long: &str) -> Result<(), String> {
    // `--name=value` split (only `--dump=` uses it today, accepted+ignored).
    let (name, _val) = long.split_once('=').unwrap_or((long, ""));
    match name {
        "version" => cli.show_version = true,
        "help" => cli.show_help = true,
        "repl" => cli.repl = true,
        "lsp" => cli.lsp = true,
        "dap" => cli.dap = true,
        "build" => cli.build = true,
        "native" => cli.native = true,
        "dump-bytecode" => cli.dump_bytecode = true,
        "dump-tokens" => cli.dump_tokens = true,
        "dump-ast" => cli.dump_ast = true,
        "disasm" => cli.disasm = true,
        // Accepted for compat, no effect: rubylang has no RubyGems to disable and
        // no external encodings to select.
        "disable-gems" | "enable-gems" | "disable-all" | "enable-all" | "dump" => {}
        "debug" => cli.debug = true,
        "verbose" => cli.warn_level = cli.warn_level.max(2),
        "copyright" => cli.show_version = true,
        other => return Err(format!("invalid option --{other}")),
    }
    Ok(())
}

/// A bundled short-switch token (already stripped of its leading `-`). Value
/// switches (`-e`/`-I`/`-r`/`-F`/`-C`/`-E`) consume the glued remainder or the
/// next token and end the cluster.
fn apply_short_cluster(
    cli: &mut Cli,
    cluster: &str,
    it: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
) -> Result<(), String> {
    let bytes: Vec<char> = cluster.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // The remainder of this token, after the current flag char.
        let rest: String = bytes[i + 1..].iter().collect();
        // For a value flag, the argument is the glued remainder or the next token.
        let take_arg = |rest: &str, it: &mut std::iter::Peekable<std::vec::IntoIter<String>>| {
            if !rest.is_empty() {
                Some(rest.to_string())
            } else {
                it.next()
            }
        };
        match c {
            'e' => {
                let arg =
                    take_arg(&rest, it).ok_or_else(|| "no code specified for -e".to_string())?;
                cli.eval.push(arg);
                return Ok(());
            }
            'I' => {
                let arg =
                    take_arg(&rest, it).ok_or_else(|| "missing argument for -I".to_string())?;
                cli.includes.push(arg);
                return Ok(());
            }
            'r' => {
                let arg =
                    take_arg(&rest, it).ok_or_else(|| "missing argument for -r".to_string())?;
                cli.requires.push(arg);
                return Ok(());
            }
            'F' => {
                let arg =
                    take_arg(&rest, it).ok_or_else(|| "missing argument for -F".to_string())?;
                cli.field_sep = Some(arg);
                cli.autosplit = true;
                return Ok(());
            }
            'c' => cli.check_syntax = true,
            'w' => cli.warn_level = cli.warn_level.max(1),
            'W' => {
                // `-W`, `-W0`, `-W1`, `-W2`; a level digit follows in the token.
                match bytes.get(i + 1).and_then(|d| d.to_digit(10)) {
                    Some(n) => {
                        cli.warn_level = n as u8;
                        i += 1;
                    }
                    None => cli.warn_level = 2,
                }
            }
            'n' => cli.loop_n = true,
            'p' => cli.loop_p = true,
            'a' => cli.autosplit = true,
            'l' => cli.chomp = true,
            'S' => cli.search_path = true,
            'd' => cli.debug = true,
            'v' => cli.verbose_version = true,
            'h' => cli.show_help = true,
            // `-0`, `-K`, `-U`, `-T`, `-x` are accepted but inert: rubylang is
            // UTF-8 only, untainted, and has no `$/` override yet.
            '0' | 'K' | 'U' | 'T' | 'x' => {}
            'C' | 'E' => {
                // Value flags accepted for compat; argument consumed, ignored.
                let _ = take_arg(&rest, it);
                return Ok(());
            }
            other => return Err(format!("invalid option -{other}")),
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Cli {
        parse_args(args.iter().map(|s| s.to_string()).collect()).unwrap()
    }

    #[test]
    fn eval_takes_argv_not_file() {
        // `-e` switches off the program-file slot: trailing tokens are ARGV.
        let c = p(&["-e", "p ARGV", "one", "two"]);
        assert_eq!(c.eval, ["p ARGV"]);
        assert_eq!(c.file, None);
        assert_eq!(c.argv, ["one", "two"]);
    }

    #[test]
    fn multiple_e_snippets_kept_in_order() {
        let c = p(&["-e", "x=1", "-e", "p x"]);
        assert_eq!(c.eval, ["x=1", "p x"]);
        assert!(c.argv.is_empty());
    }

    #[test]
    fn double_dash_ends_options() {
        let c = p(&["-e", "p ARGV", "--", "-x", "-y"]);
        assert_eq!(c.argv, ["-x", "-y"]);
    }

    #[test]
    fn glued_and_detached_value_switches() {
        let c = p(&["-Ilib", "-I", "vendor", "-rfoo", "-r", "bar", "-e", "0"]);
        assert_eq!(c.includes, ["lib", "vendor"]);
        assert_eq!(c.requires, ["foo", "bar"]);
    }

    #[test]
    fn bundled_boolean_switches() {
        let c = p(&["-wc", "-e", "0"]);
        assert_eq!(c.warn_level, 1);
        assert!(c.check_syntax);
    }

    #[test]
    fn bundled_flag_then_e_takes_next_token() {
        // `-ne 'code'` == `-n -e 'code'`; the `-e` arg is the next token.
        let c = p(&["-ne", "p 1"]);
        assert!(c.loop_n);
        assert_eq!(c.eval, ["p 1"]);
    }

    #[test]
    fn warning_level_digit() {
        assert_eq!(p(&["-W0", "-e", "0"]).warn_level, 0);
        assert_eq!(p(&["-W2", "-e", "0"]).warn_level, 2);
        assert_eq!(p(&["-W", "-e", "0"]).warn_level, 2);
    }

    #[test]
    fn file_mode_takes_first_positional_then_argv() {
        let c = p(&["script.rb", "a", "b"]);
        assert_eq!(c.file.as_deref(), Some("script.rb"));
        assert_eq!(c.argv, ["a", "b"]);
    }

    #[test]
    fn switches_before_file() {
        let c = p(&["-w", "-Ilib", "script.rb", "arg"]);
        assert_eq!(c.warn_level, 1);
        assert_eq!(c.includes, ["lib"]);
        assert_eq!(c.file.as_deref(), Some("script.rb"));
        assert_eq!(c.argv, ["arg"]);
    }

    #[test]
    fn bare_dash_is_stdin_program() {
        let c = p(&["-", "arg"]);
        assert_eq!(c.file.as_deref(), Some("-"));
        assert_eq!(c.argv, ["arg"]);
    }

    #[test]
    fn unknown_switch_is_error() {
        assert!(parse_args(vec!["-Z".to_string()]).is_err());
        assert!(parse_args(vec!["--bogus".to_string()]).is_err());
    }

    #[test]
    fn long_extensions_still_parse() {
        assert!(p(&["--repl"]).repl);
        assert!(p(&["--version"]).show_version);
        assert!(p(&["--dump-ast", "x.rb"]).dump_ast);
    }
}
