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
}
