//! End-to-end tests for `require` / `require_relative` / `load`.
//!
//! These drive the `ruby` binary against real `.rb` files under a private temp
//! directory (a fresh dir per test so parallel runs don't collide), because the
//! feature is fundamentally about resolving, reading, and running files from
//! disk and tracking the running file's directory. Each test asserts on the
//! program's stdout.
//!
//! The load-critical case is the proc/begin-id merge: a required file's block
//! and begin/rescue bodies get new ids appended above the main program's, and
//! the operands referencing them are rewritten. If that merge were wrong, a
//! required block would dispatch to the *main* program's block of the same id
//! and produce a different result — the `poison`/`transform` assertions below
//! are constructed so that a collision yields an observably wrong number.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// A unique temp directory for one test (pid + a process-wide counter).
fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rubylang_req_{}_{}_{}_{}",
        tag,
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, body).unwrap();
    p
}

/// Run `ruby <path>` and return (stdout, stderr, success).
fn run(path: &Path) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg(path)
        .output()
        .expect("spawn ruby");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

#[test]
fn require_loads_constants_classes_methods_globals() {
    let dir = fresh_dir("basic");
    write(
        &dir,
        "lib/helper.rb",
        "GREETING = \"hi\"\n\
         $helper_flag = true\n\
         class Widget\n  def label; \"W\"; end\nend\n\
         def helper_add(a, b); a + b; end\n",
    );
    let main = write(
        &dir,
        "main.rb",
        "$LOAD_PATH << __dir__ + \"/lib\"\n\
         require \"helper\"\n\
         puts GREETING\n\
         puts $helper_flag\n\
         puts Widget.new.label\n\
         puts helper_add(2, 3)\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "hi\ntrue\nW\n5\n");
}

#[test]
fn required_block_body_runs_correctly_proc_id_merge() {
    // The main program has its own block (proc id 0) that ADDS 1000. The required
    // file's method uses a block (its own id 0 pre-merge) that MULTIPLIES by 10.
    // If proc ids collided, `transform` would add 1000; correct merge multiplies.
    let dir = fresh_dir("procmerge");
    write(
        &dir,
        "lib/doubler.rb",
        "class Doubler\n  def transform(arr); arr.map { |x| x * 10 }; end\nend\n",
    );
    let main = write(
        &dir,
        "main.rb",
        "poison = [1].map { |x| x + 1000 }\n\
         $LOAD_PATH << __dir__ + \"/lib\"\n\
         require \"doubler\"\n\
         puts Doubler.new.transform([1, 2, 3]).inspect\n\
         puts poison.inspect\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "[10, 20, 30]\n[1001]\n");
}

#[test]
fn required_begin_rescue_body_runs_correctly_begin_id_merge() {
    // Main has its own begin/rescue (begin id 0, whose rescue block adds 1). The
    // required file has a begin/rescue whose rescue block (a nested map proc)
    // negates. A bad begin- or proc-id merge would run the wrong body.
    let dir = fresh_dir("beginmerge");
    write(
        &dir,
        "lib/risky.rb",
        "def safe_div(a, b)\n  begin\n    a / b\n  rescue ZeroDivisionError\n    [a].map { |x| x * -1 }.first\n  end\nend\n",
    );
    let main = write(
        &dir,
        "main.rb",
        "m = begin; raise \"x\"; rescue; [99].map { |v| v + 1 }.first; end\n\
         require_relative \"lib/risky\"\n\
         puts m\n\
         puts safe_div(10, 2)\n\
         puts safe_div(7, 0)\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "100\n5\n-7\n");
}

#[test]
fn double_require_returns_false_and_does_not_rerun() {
    let dir = fresh_dir("dedup");
    // The required file appends to a global each time its body runs.
    write(
        &dir,
        "lib/once.rb",
        "$runs ||= 0\n$runs += 1\n",
    );
    let main = write(
        &dir,
        "main.rb",
        "$LOAD_PATH << __dir__ + \"/lib\"\n\
         puts(require(\"once\"))\n\
         puts(require(\"once\"))\n\
         puts $runs\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    // First require true (ran once), second false (skipped), body ran exactly once.
    assert_eq!(stdout, "true\nfalse\n1\n");
}

#[test]
fn require_relative_resolves_against_requiring_file() {
    // main.rb is in `dir`; it require_relatives "sub/a", which require_relatives
    // "b" — resolved against sub/, not dir/. A path bug would fail to find b.
    let dir = fresh_dir("relative");
    write(&dir, "sub/b.rb", "def from_b; \"B\"; end\n");
    write(&dir, "sub/a.rb", "require_relative \"b\"\ndef from_a; from_b + \"A\"; end\n");
    let main = write(
        &dir,
        "main.rb",
        "require_relative \"sub/a\"\nputs from_a\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "BA\n");
}

#[test]
fn load_reruns_every_time_no_dedup() {
    let dir = fresh_dir("load");
    write(&dir, "counter.rb", "$n ||= 0\n$n += 1\n");
    let main = write(
        &dir,
        "main.rb",
        "$LOAD_PATH << __dir__\n\
         load \"counter.rb\"\n\
         load \"counter.rb\"\n\
         load \"counter.rb\"\n\
         puts $n\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "3\n");
}

#[test]
fn loaded_features_and_load_path_track_state() {
    let dir = fresh_dir("features");
    let helper = write(&dir, "lib/h.rb", "X = 1\n");
    let main = write(
        &dir,
        "main.rb",
        "before = $LOADED_FEATURES.length\n\
         $LOAD_PATH << __dir__ + \"/lib\"\n\
         require \"h\"\n\
         after = $LOADED_FEATURES.length\n\
         puts(after - before)\n\
         puts $LOADED_FEATURES.last\n\
         puts $LOAD_PATH.equal?($:)\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    let canon = std::fs::canonicalize(&helper).unwrap();
    assert_eq!(
        stdout,
        format!("1\n{}\ntrue\n", canon.to_string_lossy())
    );
}

#[test]
fn load_error_on_missing_file() {
    let dir = fresh_dir("missing");
    // Rescued form: LoadError is a real, catchable exception.
    let caught = write(
        &dir,
        "caught.rb",
        "begin\n  require \"no_such_lib_zzz\"\nrescue LoadError => e\n  puts \"caught: #{e.message}\"\nend\n",
    );
    let (stdout, stderr, ok) = run(&caught);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "caught: cannot load such file -- no_such_lib_zzz\n");

    // Unrescued form: terse stderr + failure exit.
    let bare = write(&dir, "bare.rb", "require \"no_such_lib_zzz\"\n");
    let (_o, stderr, ok) = run(&bare);
    assert!(!ok);
    assert!(
        stderr.contains("cannot load such file -- no_such_lib_zzz"),
        "stderr: {stderr}"
    );
}

#[test]
fn three_file_require_chain() {
    // a requires b requires c (each require_relative). Assert load order, a
    // cross-file method call chain, and the loaded-feature count.
    let dir = fresh_dir("chain");
    write(&dir, "c.rb", "$order ||= []\n$order << \"c\"\ndef from_c; \"C\"; end\n");
    write(&dir, "b.rb", "require_relative \"c\"\n$order << \"b\"\ndef from_b; from_c + \"B\"; end\n");
    let a = write(
        &dir,
        "a.rb",
        "require_relative \"b\"\n\
         $order << \"a\"\n\
         puts $order.inspect\n\
         puts from_b\n\
         puts $LOADED_FEATURES.length\n",
    );
    let (stdout, stderr, ok) = run(&a);
    assert!(ok, "stderr: {stderr}");
    // c then b then a; from_b = from_c + "B"; two features required (b and c).
    assert_eq!(stdout, "[\"c\", \"b\", \"a\"]\nCB\n2\n");
}

#[test]
fn builtin_lib_require_is_noop_true() {
    let dir = fresh_dir("builtin");
    let main = write(
        &dir,
        "main.rb",
        "puts(require \"json\")\nputs(require \"set\")\nputs(require \"securerandom\")\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "true\ntrue\ntrue\n");
}

#[test]
fn required_file_locals_do_not_leak() {
    // A local in the required file must not clobber the caller's same-named local
    // (MRI evaluates a required file at its own top-level binding).
    let dir = fresh_dir("locals");
    write(&dir, "sets.rb", "secret = 42\n");
    let main = write(
        &dir,
        "main.rb",
        "secret = 1\nrequire_relative \"sets\"\nputs secret\n",
    );
    let (stdout, stderr, ok) = run(&main);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "1\n");
}
