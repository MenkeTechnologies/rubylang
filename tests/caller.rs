//! The live call stack: `caller`, `caller_locations`, `warn(uplevel:)`,
//! `warn(category:)` and the `Warning` module, driven through the `ruby`
//! binary so the frame paths (`-e`), lines and labels are the real ones.
//! Every expected string below was produced by MRI 4.0.8 (`ruby -e …`) and
//! frozen here.

use std::process::Command;

/// Run `ruby <flags> -e <src>` and return (stdout, stderr).
fn run(flags: &[&str], src: &str) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .args(flags)
        .arg("-e")
        .arg(src)
        .output()
        .expect("spawn ruby");
    assert!(
        out.status.success(),
        "ruby failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(src: &str) -> String {
    run(&[], src).0
}

#[test]
fn caller_lists_every_waiting_method_frame_with_its_call_line() {
    let src = "def a = b\ndef b = caller(0)\np a\ndef c = caller\np c, caller(0), caller";
    assert_eq!(
        stdout_of(src),
        "[\"-e:2:in 'Object#b'\", \"-e:1:in 'Object#a'\", \"-e:3:in '<main>'\"]\n\
         [\"-e:5:in '<main>'\"]\n\
         [\"-e:5:in '<main>'\"]\n\
         []\n"
    );
}

#[test]
fn caller_locations_carry_path_line_and_labels() {
    let src = "class K\n  def self.s = caller_locations(0, 2).map { [_1.path, _1.lineno, _1.label, _1.base_label, _1.to_s] }\n  def n = K.s\nend\np K.new.n";
    assert_eq!(
        stdout_of(src),
        "[[\"-e\", 2, \"K.s\", \"s\", \"-e:2:in 'K.s'\"], [\"-e\", 3, \"K#n\", \"n\", \"-e:3:in 'K#n'\"]]\n"
    );
}

#[test]
fn blocks_are_frames_named_where_written_and_a_yielding_builtin_is_a_frame() {
    // `yield` is a call from `K#m` at its own line; `Array#each` running the
    // block gets a frame of its own at the line that called it.
    let src = "class K\n  def m = yield\nend\np K.new.m { caller(0) }\n[1].each { p caller(0) }";
    assert_eq!(
        stdout_of(src),
        "[\"-e:4:in 'block in <main>'\", \"-e:2:in 'K#m'\", \"-e:4:in '<main>'\"]\n\
         [\"-e:5:in 'block in <main>'\", \"-e:5:in 'Array#each'\", \"-e:5:in '<main>'\"]\n"
    );
}

#[test]
fn start_length_and_range_select_like_mri() {
    // A start past the end is nil, exactly at the end is [], a zero length is
    // [] regardless; negatives are ArgumentErrors.
    let src = "p caller(1..1), caller(0..0), caller(5), caller(0, 0), (caller(-1) rescue $!), (caller(0, -1) rescue $!)";
    assert_eq!(
        stdout_of(src),
        "[]\n[\"-e:1:in '<main>'\"]\nnil\n[]\n\
         #<ArgumentError: negative level (-1)>\n#<ArgumentError: negative size (-1)>\n"
    );
}

#[test]
fn a_fiber_backtrace_starts_at_its_block() {
    let src = "f = Fiber.new { caller(0) }\np f.resume";
    assert_eq!(stdout_of(src), "[\"-e:1:in 'block in <main>'\"]\n");
}

#[test]
fn caller_is_a_kernel_function_even_beside_method_missing() {
    let src = "class MM\n  def method_missing(n, *) = :mm\n  def t = caller(0, 1)\nend\np MM.new.t";
    assert_eq!(stdout_of(src), "[\"-e:3:in 'MM#t'\"]\n");
}

const WARN_SRC: &str = r#"def old_api
  warn "old_api is deprecated", uplevel: 1
end
old_api
[1].each { warn "in block", uplevel: 0 }
warn "far", uplevel: 50
warn "two", "lines", uplevel: 0
warn "hidden", category: :deprecated
warn "shown", category: :experimental
p (warn("z", uplevel: -1) rescue $!), (warn("z", category: :bogus) rescue $!), (warn("z", category: 1) rescue $!)
p Warning.categories, Warning[:deprecated], Warning[:experimental]
Warning[:deprecated] = true
warn "now shown", category: :deprecated
p (Warning["deprecated"] rescue $!), (Warning.warn(1) rescue $!)"#;

const WARN_ERRORS: &str = "#<ArgumentError: unknown category: bogus>\n\
    #<TypeError: no implicit conversion of Integer into Symbol>\n\
    [:deprecated, :experimental, :performance, :strict_unused_block]\n";

const WARN_TAIL: &str = "#<TypeError: wrong argument type String (expected Symbol)>\n\
    #<TypeError: wrong argument type Integer (expected String)>\n";

#[test]
fn warn_uplevel_prefixes_the_callers_location() {
    let (out, err) = run(&[], WARN_SRC);
    assert_eq!(
        out,
        format!("#<ArgumentError: negative level (-1)>\n{WARN_ERRORS}false\ntrue\n{WARN_TAIL}")
    );
    // `deprecated` is off by default, so "hidden" is not printed.
    assert_eq!(
        err,
        "-e:4: warning: old_api is deprecated\n\
         -e:5: warning: in block\n\
         warning: far\n\
         -e:7: warning: two\nlines\n\
         shown\n\
         now shown\n"
    );
}

#[test]
fn w0_silences_warn_and_every_category() {
    let (out, err) = run(&["-W0"], WARN_SRC);
    // `$VERBOSE` nil returns before `uplevel:` is even read.
    assert_eq!(out, format!("nil\n{WARN_ERRORS}false\nfalse\n{WARN_TAIL}"));
    assert_eq!(err, "");
}

#[test]
fn dash_w_turns_deprecated_on() {
    let (out, err) = run(&["-w"], WARN_SRC);
    assert_eq!(
        out,
        format!("#<ArgumentError: negative level (-1)>\n{WARN_ERRORS}true\ntrue\n{WARN_TAIL}")
    );
    assert!(err.contains("lines\nhidden\nshown\n"), "stderr was: {err}");
}

#[test]
fn warn_goes_through_a_redefined_warning_warn() {
    let src = "module Warning\n  def self.warn(msg, category: nil) = print(\"HOOK #{category.inspect} #{msg}\")\nend\nwarn \"a\"\nwarn \"b\", category: :experimental\nwarn \"c\", uplevel: 0";
    let (out, err) = run(&[], src);
    assert_eq!(
        out,
        "HOOK nil a\nHOOK :experimental b\nHOOK nil -e:6: warning: c\n"
    );
    assert_eq!(err, "");
}
