//! Uncaught-exception stderr formatting, driven through the `ruby` binary.
//!
//! An uncaught Ruby exception must print in MRI's shape
//! (`<src>:<line>:in '<ctx>': <msg> (<Class>)` followed by tab-indented
//! `from <src>:<line>:in '<ctx>'` backtrace lines) and exit non-zero — not the
//! old terse `ruby: <msg>`. The exact strings below were produced by MRI 4.0.6
//! (`ruby -e …` / `ruby file.rb`) and frozen here; a regression in the printer,
//! the per-op line tagging, or the frame-context labels fails this test.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_file(tag: &str, body: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "rubylang_uncaught_{}_{}_{}.rb",
        tag,
        std::process::id(),
        n
    ));
    std::fs::write(&p, body).unwrap();
    p
}

/// Run `ruby -e <src>` and return (stderr, success).
fn run_e(src: &str) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg("-e")
        .arg(src)
        .output()
        .expect("spawn ruby");
    (
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

/// Run `ruby <path>` and return (stderr, success).
fn run_file(path: &Path) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg(path)
        .output()
        .expect("spawn ruby");
    (
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

#[test]
fn top_level_raise_prints_mri_shape() {
    // The task's canonical verification case.
    let (stderr, ok) = run_e(r#"raise "x""#);
    assert!(!ok, "an uncaught raise must exit non-zero");
    assert_eq!(stderr, "-e:1:in '<main>': x (RuntimeError)\n");
}

#[test]
fn raise_with_explicit_class_shows_that_class() {
    let (stderr, ok) = run_e(r#"raise ArgumentError, "nope""#);
    assert!(!ok);
    assert_eq!(stderr, "-e:1:in '<main>': nope (ArgumentError)\n");
}

#[test]
fn raise_inside_method_has_backtrace_from_line() {
    // Innermost frame first (`Object#f`), then a `from` line for `<main>`.
    let (stderr, ok) = run_e("def f; raise \"z\"; end; f");
    assert!(!ok);
    assert_eq!(
        stderr,
        "-e:1:in 'Object#f': z (RuntimeError)\n\tfrom -e:1:in '<main>'\n"
    );
}

#[test]
fn multi_frame_backtrace_reports_each_call_line() {
    // A three-deep chain across real source lines: the message line is where the
    // raise fires, each `from` line is where that frame made its call.
    let src = "def outer\n  inner\nend\ndef inner\n  raise \"deep\"\nend\nouter\n";
    let path = fresh_file("chain", src);
    let (stderr, ok) = run_file(&path);
    assert!(!ok);
    let base = path.to_string_lossy();
    assert_eq!(
        stderr,
        format!(
            "{base}:5:in 'Object#inner': deep (RuntimeError)\n\
             \tfrom {base}:2:in 'Object#outer'\n\
             \tfrom {base}:7:in '<main>'\n"
        )
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn no_ruby_prefix_on_uncaught_exception() {
    // The old printer emitted `ruby: <msg>`; MRI never prefixes a program
    // exception, so the stderr must not start with `ruby:`.
    let (stderr, ok) = run_e(r#"raise "boom""#);
    assert!(!ok);
    assert!(
        !stderr.starts_with("ruby:"),
        "unexpected ruby: prefix: {stderr:?}"
    );
    // The absence check alone cannot fail for the right reason: EMPTY stderr,
    // or any wrong diagnostic, also fails to start with `ruby:`. Pin what the
    // line must be, so removing the prefix by removing the printer is caught.
    assert_eq!(stderr, "-e:1:in '<main>': boom (RuntimeError)\n");
}

/// A frame inside a BLOCK is named `block in X`, and one inside nested blocks
/// `block (N levels) in X`, where N counts the block literals the code is
/// written inside and X is the method the outermost of them was written in.
///
/// This is MRI's own naming and it is lexical: `pr` written at the top level and
/// called from inside a method is still `block in <main>`. A method CALLED from
/// inside a block is named for the method, not the block, which is why the label
/// is pushed with the frame depth it was entered at.
///
/// Every expectation is the reference `ruby`'s, taken from `e.backtrace.first`
/// on the same shapes (`ruby 4.0.6`); the uncaught printer puts the same label
/// in its first line.
#[test]
fn a_block_frame_is_named_block_in_its_enclosing_context() {
    for (src, want) in [
        // No block at all.
        (r#"raise "x""#, "'<main>'"),
        // One, two, three deep.
        (r#"[1].each { raise "x" }"#, "'block in <main>'"),
        (
            r#"[1].each { [2].each { raise "x" } }"#,
            "'block (2 levels) in <main>'",
        ),
        (
            r#"[1].each { [2].each { [3].each { raise "x" } } }"#,
            "'block (3 levels) in <main>'",
        ),
        // The enclosing context is the method the block was written in — `#` for
        // an instance method, `.` for a singleton one.
        (
            "class K\n  def m = [1].each { raise \"x\" }\nend\nK.new.m",
            "'block in K#m'",
        ),
        (
            "class K\n  def self.s = [1].each { raise \"x\" }\nend\nK.s",
            "'block in K.s'",
        ),
        (
            "class K\n  def d = [1].each { [2].each { raise \"x\" } }\nend\nK.new.d",
            "'block (2 levels) in K#d'",
        ),
        // A method called FROM a block is named for the method: the block's
        // label is not the innermost frame any more.
        (
            "def where = raise(\"x\")\n[1].each { where }",
            "'Object#where'",
        ),
        // A lambda body is a block body too.
        (r#"l = lambda { raise "x" }; l.call"#, "'block in <main>'"),
    ] {
        let (stderr, ok) = run_e(src);
        assert!(!ok, "an uncaught raise must exit non-zero: {src}");
        let got = stderr
            .split_once(":in ")
            .and_then(|(_, rest)| rest.split_once(':'))
            .map(|(label, _)| label.to_string())
            .unwrap_or_else(|| format!("(no label in {stderr:?})"));
        assert_eq!(got, want, "for {src}");
    }
}

/// An operator that raises reports the line it is written on.
///
/// `**`, `/`, `%`, `<<`, `>>`, `<=>`, `===`, `=~`, `&`, `|` and `^` are Ruby
/// METHODS, so the compiler routes them through method dispatch rather than
/// fusevm's native op — and that one dispatch site emitted its op with line `0`
/// where every other call site passes the line being compiled. `p 1/0` reported
/// `-e:0:in '<main>'` where MRI 4.0.6 reports line 1, and so did every other
/// operator in that list. Nothing else about the message changed, which is why
/// these pin the whole string: the line is what regressed and the rest is what
/// must not move with it.
///
/// The frame is still `<main>` where MRI names the builtin that raised
/// (`Integer#/`, and a `from` line for the caller). That gap is separate and is
/// recorded in BUGS.md; a USER method's frame is already named, which
/// `raise_inside_method_has_backtrace_from_line` pins and the last case here
/// re-checks for the operator path.
#[test]
fn an_operator_that_raises_reports_its_own_line() {
    let (stderr, ok) = run_e("p 1/0");
    assert!(!ok);
    assert_eq!(
        stderr,
        "-e:1:in '<main>': divided by 0 (ZeroDivisionError)\n"
    );

    let (stderr, ok) = run_e("# a\n# b\np 1/0");
    assert!(!ok);
    assert_eq!(
        stderr,
        "-e:3:in '<main>': divided by 0 (ZeroDivisionError)\n"
    );

    let (stderr, ok) = run_e("# a\np 1 % 0");
    assert!(!ok);
    assert_eq!(
        stderr,
        "-e:2:in '<main>': divided by 0 (ZeroDivisionError)\n"
    );

    let (stderr, ok) = run_e("# a\np 10 ** 10 ** 10");
    assert!(!ok);
    assert_eq!(
        stderr,
        "-e:2:in '<main>': exponent is too large (ArgumentError)\n"
    );

    // Inside a method the operator's line is the body's, and the `from` line is
    // the call — the same shape a `raise` there produces.
    let (stderr, ok) = run_e("# a\ndef f(x)\n  x/0\nend\nf(1)");
    assert!(!ok);
    assert_eq!(
        stderr,
        "-e:3:in 'Object#f': divided by 0 (ZeroDivisionError)\n\tfrom -e:5:in '<main>'\n"
    );
}
