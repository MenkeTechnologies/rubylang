//! What the ENTRY POINT changes about a program's view of itself.
//!
//! The same source answers differently depending on how it was handed to the
//! interpreter — `-e`, a script file, or stdin — and a switch (`-w`, `-W`,
//! `RUBYOPT`) can change an observable without changing a line of the program.
//! Those differences are easy to get wrong in one direction only, and a test
//! that pins just one entry point cannot see it: a harness that always uses
//! `-e` will happily agree with a `__dir__` that is wrong for script files, and
//! vice versa. So each observable here is pinned at EVERY entry point that
//! reaches it, not at whichever one was convenient.
//!
//! Every expectation was measured against the reference MRI at
//! `/opt/homebrew/opt/ruby/bin/ruby` — `ruby 4.0.6 (2026-07-14 revision
//! 03b6d3f889) +PRISM [arm64-darwin25]`. It is named by absolute path on
//! purpose: rubylang installs its own binary as `ruby`, so a bare `ruby` on
//! `PATH` is rubylang and would compare it against itself. See `src/oracle.rs`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// A unique temp directory for one test, so parallel runs cannot collide.
fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rubylang_entry_{}_{}_{}",
        tag,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `ruby <args…>`, with `stdin` fed in and `RUBYOPT` optionally set.
/// Returns `(stdout, stderr, exit-success)`.
fn run(args: &[&str], stdin: &str, rubyopt: Option<&str>) -> (String, String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ruby"));
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match rubyopt {
        Some(v) => cmd.env("RUBYOPT", v),
        // Inherited RUBYOPT would silently change what these tests measure.
        None => cmd.env_remove("RUBYOPT"),
    };
    let mut child = cmd.spawn().expect("spawn ruby");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait for ruby");
    (
        String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        out.status.success(),
    )
}

/// Run `src` through `-e`, through a script file, and through stdin, returning
/// the three stdouts in that order. The script path is returned too, because
/// the file answer is usually expressed in terms of it.
fn each_entry_point(tag: &str, src: &str) -> (String, String, String, PathBuf) {
    let dir = fresh_dir(tag);
    let path = dir.join("prog.rb");
    std::fs::write(&path, format!("{src}\n")).unwrap();
    let p = path.to_str().unwrap();
    let (dash_e, _, ok_e) = run(&["-e", src], "", None);
    let (file, _, ok_f) = run(&[p], "", None);
    let (stdin, _, ok_s) = run(&[], &format!("{src}\n"), None);
    assert!(
        ok_e && ok_f && ok_s,
        "all three entry points should succeed"
    );
    (dash_e, file, stdin, path)
}

#[test]
fn program_name_and_file_name_the_entry_point() {
    // MRI: `-e` reports the literal "-e", a script file reports the path
    // EXACTLY as written on the command line (it is not canonicalized), and a
    // program read from stdin reports "-".
    let (dash_e, file, stdin, path) = each_entry_point("names", "p [$0, __FILE__, $PROGRAM_NAME]");
    assert_eq!(dash_e, r#"["-e", "-e", "-e"]"#);
    let p = path.to_str().unwrap();
    assert_eq!(file, format!(r#"["{p}", "{p}", "{p}"]"#));
    assert_eq!(stdin, r#"["-", "-", "-"]"#);
}

#[test]
fn dir_is_dot_where_the_program_has_no_file() {
    // `__dir__` is `File.dirname(File.realpath(__FILE__))`. Under `-e` and
    // stdin `__FILE__` names no file, so the realpath step drops out and MRI
    // answers the relative `"."` — NOT the current directory spelled out. A
    // file-based program still gets its own absolute directory.
    let (dash_e, file, stdin, path) = each_entry_point("dir", "p __dir__");
    assert_eq!(dash_e, r#"".""#);
    assert_eq!(stdin, r#"".""#);
    let want = std::fs::canonicalize(path.parent().unwrap()).unwrap();
    assert_eq!(file, format!(r#""{}""#, want.display()));
}

#[test]
fn dir_being_dot_does_not_break_require_relative() {
    // The `"."` above is what `__dir__` REPORTS; the directory the interpreter
    // resolves against is still the real one. Conflating the two would make a
    // `-e` one-liner unable to require anything next to it.
    let dir = fresh_dir("reqrel");
    std::fs::write(dir.join("side.rb"), "SIDE = 7\n").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ruby"));
    let out = cmd
        .current_dir(&dir)
        .env_remove("RUBYOPT")
        .args(["-e", "require_relative 'side'; p [SIDE, __dir__]"])
        .output()
        .expect("spawn ruby");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        r#"[7, "."]"#
    );
}

#[test]
fn a_bare_dash_names_stdin_as_the_program() {
    // `ruby -` reads the PROGRAM from stdin. It must not be read as a request
    // to open a file literally called "-", and the tokens after it are ARGV,
    // not more files.
    let (out, err, ok) = run(&["-", "one", "two"], "p [$0, __FILE__, ARGV]\n", None);
    assert!(ok, "`ruby -` should run, stderr: {err}");
    assert_eq!(out, r#"["-", "-", ["one", "two"]]"#);
}

#[test]
fn verbose_is_false_by_default_and_true_only_when_asked() {
    // MRI's starting state is `$VERBOSE = false`. `nil` is a DIFFERENT state
    // that only `-W0` selects, so "no switch given" must not collapse into it.
    for (args, want) in [
        (vec!["-e", "p $VERBOSE"], "false"),
        (vec!["-w", "-e", "p $VERBOSE"], "true"),
        (vec!["-W", "-e", "p $VERBOSE"], "true"),
        (vec!["-W0", "-e", "p $VERBOSE"], "nil"),
        (vec!["-W1", "-e", "p $VERBOSE"], "false"),
        (vec!["-W2", "-e", "p $VERBOSE"], "true"),
        // MRI scans the level as octal, so 7 is still a level.
        (vec!["-W7", "-e", "p $VERBOSE"], "true"),
        (vec!["--verbose", "-e", "p $VERBOSE"], "true"),
    ] {
        let (out, err, ok) = run(&args, "", None);
        assert!(ok, "{args:?} should run, stderr: {err}");
        assert_eq!(out, want, "for {args:?}");
    }
}

#[test]
fn the_last_warning_switch_wins_rather_than_the_highest() {
    // `-w -W0` ends at nil and `-W0 -w` ends at true. Treating the level as a
    // high-water mark would make both `true` and silently ignore the `-W0`.
    for (args, want) in [
        (vec!["-w", "-W0", "-e", "p $VERBOSE"], "nil"),
        (vec!["-W0", "-w", "-e", "p $VERBOSE"], "true"),
        (vec!["-w", "-W1", "-e", "p $VERBOSE"], "false"),
        (vec!["-W2", "-W0", "-e", "p $VERBOSE"], "nil"),
    ] {
        let (out, _, ok) = run(&args, "", None);
        assert!(ok, "{args:?} should run");
        assert_eq!(out, want, "for {args:?}");
    }
}

#[test]
fn a_non_octal_warning_digit_is_not_a_level() {
    // `-W8` is not `-W` at level 8: MRI reads the digit with `scan_oct`, which
    // consumes nothing, so `-W` takes its default and the stray `8` is re-read
    // as its own switch — and there is no `-8`.
    for bad in ["-W8", "-W9"] {
        let (_, err, ok) = run(&[bad, "-e", "0"], "", None);
        assert!(!ok, "{bad} should be rejected");
        assert!(
            err.contains(&format!("invalid option -{}", &bad[2..])),
            "{bad} should report the stray digit, got: {err}"
        );
    }
}

#[test]
fn an_unknown_warning_category_warns_but_still_runs() {
    // `-W:` selects a category, never a level, so it leaves `$VERBOSE` alone.
    let (out, err, ok) = run(&["-W:no-deprecated", "-e", "p $VERBOSE"], "", None);
    assert!(ok);
    assert_eq!(out, "false");
    assert_eq!(err, "");
    // An unrecognised category is a warning, not a failure.
    let (out, err, ok) = run(&["-W:bogus", "-e", "p $VERBOSE"], "", None);
    assert!(ok, "an unknown category should not stop the program");
    assert_eq!(out, "false");
    assert_eq!(err, "ruby: warning: unknown warning category: 'bogus'");
}

#[test]
fn rubyopt_applies_first_and_loses_to_the_command_line() {
    // The environment sets the baseline…
    let (out, _, ok) = run(&["-e", "p $VERBOSE"], "", Some("-w"));
    assert!(ok);
    assert_eq!(out, "true");
    // …and an explicit switch overrides it, because RUBYOPT is applied BEFORE
    // the real arguments rather than after.
    let (out, _, ok) = run(&["-W0", "-e", "p $VERBOSE"], "", Some("-w"));
    assert!(ok);
    assert_eq!(out, "nil");
}

#[test]
fn rubyopt_refuses_the_switches_that_would_hijack_the_program() {
    // `RUBYOPT` is ambient, so MRI allows only switches that cannot change WHAT
    // runs. `-e` in the environment would replace the program outright; the
    // line-loop and syntax-check switches would change the whole execution
    // mode. Each is rejected by name.
    for bad in ["-e", "-n", "-p", "-a", "-l", "-c", "-S", "-x", "-0"] {
        let (_, err, ok) = run(&["-e", "p 1"], "", Some(bad));
        assert!(!ok, "RUBYOPT={bad} should be refused");
        assert_eq!(err, format!("ruby: invalid switch in RUBYOPT: {bad}"));
    }
    for bad in ["--version", "--help"] {
        let (_, err, ok) = run(&["-e", "p 1"], "", Some(bad));
        assert!(!ok, "RUBYOPT={bad} should be refused");
        assert_eq!(err, format!("ruby: invalid switch in RUBYOPT: {bad}"));
    }
    // The allowed ones still work, including a value switch and a bundle.
    let (out, err, ok) = run(
        &["-e", "p [$VERBOSE, $LOAD_PATH.include?('/tmp')]"],
        "",
        Some("-w -I/tmp"),
    );
    assert!(ok, "stderr: {err}");
    assert_eq!(out, "[true, true]");
}

#[test]
fn an_uncaught_error_is_labelled_with_the_entry_point() {
    // The `file:line:in '<main>'` prefix names the program the same way
    // `__FILE__` does, so the label is entry-point-sensitive too.
    let dir = fresh_dir("raise");
    let path = dir.join("boom.rb");
    std::fs::write(&path, "raise \"boom\"\n").unwrap();

    let (_, err, ok) = run(&["-e", "raise \"boom\""], "", None);
    assert!(!ok);
    assert!(
        err.starts_with("-e:1:in '<main>': boom (RuntimeError)"),
        "got: {err}"
    );

    let p = path.to_str().unwrap();
    let (_, err, ok) = run(&[p], "", None);
    assert!(!ok);
    assert!(
        err.starts_with(&format!("{p}:1:in '<main>': boom (RuntimeError)")),
        "got: {err}"
    );

    let (_, err, ok) = run(&[], "raise \"boom\"\n", None);
    assert!(!ok);
    assert!(
        err.starts_with("-:1:in '<main>': boom (RuntimeError)"),
        "got: {err}"
    );
}

#[test]
fn top_level_self_prints_as_main_at_every_entry_point() {
    // `main` is an ordinary Object, but it prints as `main` — and it is the
    // same object however the program was started.
    let (dash_e, file, stdin, _) =
        each_entry_point("main", "p [self.to_s, self.inspect, self.class.name]");
    let want = r#"["main", "main", "Object"]"#;
    assert_eq!(dash_e, want);
    assert_eq!(file, want);
    assert_eq!(stdin, want);
}
