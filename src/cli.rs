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
    /// MRI verbosity level: 0 (`-W0`) → `$VERBOSE = nil`, 1 (no switch) →
    /// `false`, ≥2 (`-w`, `-v`, `-W2`…`-W7`, `--verbose`) → `true`. Switches
    /// ASSIGN this rather than raising it, because MRI is last-wins: `-w -W0`
    /// ends at `nil` and `-W0 -w` ends at `true`.
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

    /// Warnings the parser itself produced (today: `-W:` naming an unknown
    /// warning category). Collected rather than printed so parsing stays pure
    /// and testable; `main` writes them to stderr before the program runs.
    pub warnings: Vec<String>,

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
    /// `--tiers`: run the script, then report which fusevm tiers took its chunks.
    pub tiers: bool,
}

/// The level `-w`, `-v`, `-W` and `--verbose` all select: `$VERBOSE = true`.
const VERBOSE_LEVEL: u8 = 2;

/// The warning categories `-W:` and `-W:no-` accept. An unrecognised one is a
/// warning, not an error, and leaves the level alone. Verified against MRI
/// 4.0.6: `-W:foo` prints `warning: unknown warning category: 'foo'` and runs.
const WARNING_CATEGORIES: &[&str] = &[
    "deprecated",
    "experimental",
    "performance",
    "strict_unused_block",
];

/// Short switches MRI accepts inside `RUBYOPT`. Every other switch is rejected
/// there even though it is valid on the command line, because `RUBYOPT` is
/// ambient: a stray `-e` in the environment would silently replace the program.
/// MRI's `proc_options` marks the rest `noenvopt`. Verified against MRI 4.0.6 —
/// `-a`/`-n`/`-p`/`-l`/`-c`/`-S`/`-x`/`-0`/`-e` all answer
/// `invalid switch in RUBYOPT`, while these nine run.
const ENV_SHORT: &str = "IdvwWrKUE";

/// Short switches in [`ENV_SHORT`] that swallow the rest of their token (a
/// value, or `-W`'s level digit / `:category`), so the characters after them
/// are an argument and must not be checked as further switches.
const ENV_SHORT_TAKES_REST: &str = "IrEW";

/// `--long` switches MRI accepts inside `RUBYOPT`, beyond the `--enable-*` /
/// `--disable-*` families. `--version` and `--help` are rejected there.
const ENV_LONG: &[&str] = &["verbose", "debug"];

/// Split `RUBYOPT` into switch tokens, rejecting any MRI would not honour.
///
/// The tokens are returned rather than applied so they can be placed *before*
/// the real command line, which is where MRI puts them: an explicit `-W0` on
/// the command line overrides a `-w` from the environment, not the reverse.
pub fn split_env_options(rubyopt: &str) -> Result<Vec<String>, String> {
    let bad = |tok: &str| format!("invalid switch in RUBYOPT: {tok}");
    let mut out = Vec::new();
    for tok in rubyopt.split_whitespace() {
        if let Some(long) = tok.strip_prefix("--") {
            let name = long.split_once('=').map_or(long, |(n, _)| n);
            let ok = ENV_LONG.contains(&name)
                || name.starts_with("enable-")
                || name.starts_with("disable-");
            if !ok {
                return Err(bad(tok));
            }
        } else if let Some(short) = tok.strip_prefix('-') {
            if short.is_empty() {
                return Err(bad(tok));
            }
            for c in short.chars() {
                if !ENV_SHORT.contains(c) {
                    return Err(bad(&format!("-{c}")));
                }
                if ENV_SHORT_TAKES_REST.contains(c) {
                    break;
                }
            }
        } else {
            // A bare word in RUBYOPT would be a program file or an argument;
            // neither can come from the environment.
            return Err(bad(tok));
        }
        out.push(tok.to_string());
    }
    Ok(out)
}

/// Parse the real process arguments (skipping `argv[0]`), with any `RUBYOPT`
/// switches applied first.
pub fn parse() -> Result<Cli, String> {
    let mut args = match std::env::var("RUBYOPT") {
        Ok(opt) => split_env_options(&opt)?,
        Err(_) => Vec::new(),
    };
    args.extend(std::env::args().skip(1));
    parse_args(args)
}

/// Parse an explicit argument vector (testable, no `argv[0]`).
pub fn parse_args(args: Vec<String>) -> Result<Cli, String> {
    let mut cli = Cli {
        warn_level: crate::DEFAULT_WARN_LEVEL,
        ..Cli::default()
    };
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
        "tiers" => cli.tiers = true,
        // Accepted for compat, no effect: rubylang has no RubyGems to disable and
        // no external encodings to select.
        "disable-gems" | "enable-gems" | "disable-all" | "enable-all" | "dump" => {}
        "debug" => cli.debug = true,
        "verbose" => cli.warn_level = VERBOSE_LEVEL,
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
            // `-w` is `$VERBOSE = true`, i.e. level 2 — the same state `-W2`
            // names. It is an assignment, not a raise: `-w -W0` ends at `nil`.
            'w' => cli.warn_level = VERBOSE_LEVEL,
            'W' => {
                // `-W:category` selects a warning category instead of a level.
                // The category runs to the end of the token.
                if bytes.get(i + 1) == Some(&':') {
                    let cat: String = bytes[i + 2..].iter().collect();
                    if !WARNING_CATEGORIES.contains(&cat.trim_start_matches("no-")) {
                        cli.warnings
                            .push(format!("warning: unknown warning category: '{cat}'"));
                    }
                    return Ok(());
                }
                // `-W`, `-W0`…`-W7`. MRI scans the level with `scan_oct`, so 8
                // and 9 are NOT digits here: the `W` takes its default level and
                // the stray digit is re-read as its own (invalid) switch.
                match bytes.get(i + 1).and_then(|d| d.to_digit(8)) {
                    Some(n) => {
                        cli.warn_level = n as u8;
                        i += 1;
                    }
                    None => cli.warn_level = VERBOSE_LEVEL,
                }
            }
            'n' => cli.loop_n = true,
            'p' => cli.loop_p = true,
            'a' => cli.autosplit = true,
            'l' => cli.chomp = true,
            'S' => cli.search_path = true,
            'd' => cli.debug = true,
            // `-v` prints the banner AND runs verbosely — it is `--verbose` with
            // a version line, not a bare informational switch.
            'v' => {
                cli.verbose_version = true;
                cli.warn_level = VERBOSE_LEVEL;
            }
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
    use crate::DEFAULT_WARN_LEVEL;

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
        // Both flags in the cluster take effect. `-w` is the verbose level, not
        // level 1: MRI 4.0.6 answers `true` for `ruby -w -e 'p $VERBOSE'`, and
        // level 1 is `false`.
        let c = p(&["-wc", "-e", "0"]);
        assert_eq!(c.warn_level, VERBOSE_LEVEL);
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
    fn no_switch_is_level_one_not_zero() {
        // The state MRI starts in is `$VERBOSE = false` (level 1). Level 0 is
        // `nil` and is reachable only by asking: a derived `Default` would make
        // "no switch" and `-W0` the same state.
        assert_eq!(p(&["-e", "0"]).warn_level, DEFAULT_WARN_LEVEL);
        assert_eq!(DEFAULT_WARN_LEVEL, 1);
        assert_ne!(
            p(&["-e", "0"]).warn_level,
            p(&["-W0", "-e", "0"]).warn_level
        );
    }

    #[test]
    fn verbose_switches_all_reach_the_same_level() {
        for args in [
            vec!["-w", "-e", "0"],
            vec!["-v", "-e", "0"],
            vec!["--verbose", "-e", "0"],
            vec!["-W", "-e", "0"],
            vec!["-W2", "-e", "0"],
        ] {
            assert_eq!(p(&args).warn_level, VERBOSE_LEVEL, "{args:?}");
        }
        // `-v` also still asks for the banner.
        assert!(p(&["-v", "-e", "0"]).verbose_version);
    }

    #[test]
    fn warning_level_is_last_wins_not_highest() {
        // MRI 4.0.6: `-w -W0` → nil, `-W0 -w` → true, `-w -W1` → false. A `max`
        // would make all three `true`.
        assert_eq!(p(&["-w", "-W0", "-e", "0"]).warn_level, 0);
        assert_eq!(p(&["-W0", "-w", "-e", "0"]).warn_level, VERBOSE_LEVEL);
        assert_eq!(p(&["-w", "-W1", "-e", "0"]).warn_level, 1);
        assert_eq!(p(&["-W2", "-W0", "-e", "0"]).warn_level, 0);
    }

    #[test]
    fn warning_level_digit_is_octal() {
        // MRI scans the `-W` level with `scan_oct`, so 0..=7 are levels…
        for n in 0..=7u8 {
            assert_eq!(p(&[&format!("-W{n}"), "-e", "0"]).warn_level, n);
        }
        // …and 8/9 are not digits at all: `-W` takes its default and the stray
        // character is re-read as its own switch, which is invalid.
        for bad in ["-W8", "-W9"] {
            assert!(
                parse_args(vec![bad.to_string(), "-e".into(), "0".into()]).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn warning_category_switch_does_not_change_the_level() {
        let c = p(&["-W:no-deprecated", "-e", "0"]);
        assert_eq!(c.warn_level, DEFAULT_WARN_LEVEL);
        assert!(c.warnings.is_empty());
        // An unknown category warns but still runs.
        let c = p(&["-W:bogus", "-e", "0"]);
        assert_eq!(c.warn_level, DEFAULT_WARN_LEVEL);
        assert_eq!(c.warnings, ["warning: unknown warning category: 'bogus'"]);
    }

    #[test]
    fn rubyopt_accepts_only_the_switches_mri_allows() {
        // Verified against MRI 4.0.6 by setting RUBYOPT and reading the result.
        for ok in [
            "-w", "-W0", "-v", "-d", "-I/tmp", "-rjson", "-K", "-U", "-E:UTF-8",
        ] {
            assert!(split_env_options(ok).is_ok(), "{ok} should be allowed");
        }
        for bad in ["-a", "-n", "-p", "-l", "-c", "-S", "-x", "-0", "-e"] {
            let err = split_env_options(bad).unwrap_err();
            assert_eq!(err, format!("invalid switch in RUBYOPT: {bad}"));
        }
        // Long switches: the enable/disable families and these two only.
        for ok in [
            "--verbose",
            "--debug",
            "--disable-gems",
            "--enable-frozen-string-literal",
        ] {
            assert!(split_env_options(ok).is_ok(), "{ok} should be allowed");
        }
        for bad in ["--version", "--help"] {
            assert!(split_env_options(bad).is_err(), "{bad} should be rejected");
        }
        // A bare word could only be a program file; the environment cannot name one.
        assert!(split_env_options("script.rb").is_err());
    }

    #[test]
    fn rubyopt_switches_lose_to_the_command_line() {
        // MRI applies RUBYOPT first, so an explicit switch overrides it:
        // `RUBYOPT=-w ruby -W0 -e 'p $VERBOSE'` prints nil.
        let mut args = split_env_options("-w").unwrap();
        args.extend(["-W0", "-e", "0"].iter().map(|s| s.to_string()));
        assert_eq!(parse_args(args).unwrap().warn_level, 0);
        // …and with nothing overriding it, the environment switch stands.
        let mut args = split_env_options("-w -I/tmp").unwrap();
        args.extend(["-e", "0"].iter().map(|s| s.to_string()));
        let c = parse_args(args).unwrap();
        assert_eq!(c.warn_level, VERBOSE_LEVEL);
        assert_eq!(c.includes, ["/tmp"]);
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
        assert_eq!(c.warn_level, VERBOSE_LEVEL);
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
