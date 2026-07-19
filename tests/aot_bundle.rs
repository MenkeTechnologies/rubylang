//! End-to-end tests for build-time `require` bundling (`ruby --build`).
//!
//! `--build` statically resolves every literal `require`/`require_relative` from
//! the entrypoint, inlines each required file, and stores the whole app as one
//! compiled program in the cache. A subsequent `ruby FILE` then runs that bundle
//! directly — even with the required source files deleted.
//!
//! Each test drives the real `ruby` binary against files in a private temp dir,
//! with `HOME` pointed at an isolated cache so it neither pollutes nor races the
//! developer's real `~/.rubylang` shard (and parallel test processes stay
//! independent). The load-bearing assertion is that three outputs agree:
//!   1. running the entry directly (runtime `require`),
//!   2. running the bundle from cache after `--build` with the required sources
//!      deleted (proves the whole app is in the artifact),
//!   3. the reference `ruby` (MRI), when available.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rubylang_aot_{}_{}_{}_{}",
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

/// Run `ruby [args…] FILE` with an isolated `HOME` (so the cache is per-test).
/// Returns (stdout, stderr, success).
fn run_home(home: &Path, args: &[&str], path: &Path) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .env("HOME", home)
        .args(args)
        .arg(path)
        .output()
        .expect("spawn ruby");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

/// The reference MRI output, if `/opt/homebrew/bin/ruby` is present. Used as a
/// third witness; the rubylang-vs-rubylang equality is asserted unconditionally.
fn mri(path: &Path) -> Option<String> {
    let out = Command::new("/opt/homebrew/bin/ruby").arg(path).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

#[test]
fn bundles_a_three_file_app_and_runs_from_cache_without_sources() {
    let dir = fresh_dir("threefile");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // other.rb: a class whose method uses a block (exercises proc lowering across
    // files) and cross-references a constant defined in the middle file.
    write(
        &dir,
        "other.rb",
        "class Greeter\n\
         def initialize(name); @name = name; end\n\
         def greet; (1..3).map { |n| \"#{GREETING_PREFIX} #{@name} #{n}\" }.join(\", \"); end\n\
         end\n",
    );
    // lib.rb: requires other.rb, defines a helper method + the shared constant.
    write(
        &dir,
        "lib.rb",
        "require_relative \"other\"\n\
         GREETING_PREFIX = \"hello\"\n\
         def build_greeter(name); Greeter.new(name); end\n",
    );
    // app.rb: the entrypoint — requires lib.rb (which requires other.rb) plus a
    // builtin lib (json, must stay a runtime no-op, never bundled).
    let app = write(
        &dir,
        "app.rb",
        "require_relative \"lib\"\n\
         require \"json\"\n\
         puts build_greeter(\"world\").greet\n",
    );

    let expected = "hello world 1, hello world 2, hello world 3\n";

    // 1. Direct run (runtime require).
    let (direct, stderr, ok) = run_home(&home, &[], &app);
    assert!(ok, "direct run failed: {stderr}");
    assert_eq!(direct, expected, "direct output");

    // 2. Build the bundle, then delete the required sources and run from cache.
    let (report, berr, bok) = run_home(&home, &["--build"], &app);
    assert!(bok, "build failed: {berr}");
    assert!(
        report.contains("3 file(s) bundled"),
        "report should mention 3 bundled files: {report}"
    );
    assert!(
        report.contains("runtime require"),
        "report should note the json runtime require: {report}"
    );

    std::fs::remove_file(dir.join("lib.rb")).unwrap();
    std::fs::remove_file(dir.join("other.rb")).unwrap();
    let (cached, cerr, cok) = run_home(&home, &[], &app);
    assert!(cok, "cached run failed (sources deleted): {cerr}");
    assert_eq!(cached, expected, "bundled/cached output with sources deleted");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn mri_parity_for_bundled_app() {
    // A self-contained variant kept intact for MRI (which needs the sources on
    // disk), asserting rubylang-bundled == rubylang-direct == MRI.
    let dir = fresh_dir("parity");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();

    write(&dir, "c.rb", "def from_c; \"C\"; end\n");
    write(
        &dir,
        "b.rb",
        "require_relative \"c\"\n\
         class Chain\n  def run; [from_c].map { |s| s + \"B\" }.first; end\nend\n",
    );
    let a = write(
        &dir,
        "a.rb",
        "require_relative \"b\"\nputs Chain.new.run\n",
    );

    let (direct, e1, o1) = run_home(&home, &[], &a);
    assert!(o1, "{e1}");
    let (_r, e2, o2) = run_home(&home, &["--build"], &a);
    assert!(o2, "{e2}");
    let (bundled, e3, o3) = run_home(&home, &[], &a);
    assert!(o3, "{e3}");

    assert_eq!(direct, bundled, "direct vs bundled");
    if let Some(ref_out) = mri(&a) {
        assert_eq!(bundled, ref_out, "bundled vs MRI");
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stale_bundle_is_rejected_after_dependency_edit() {
    let dir = fresh_dir("stale");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();

    write(&dir, "dep.rb", "VALUE = 100\n");
    let app = write(&dir, "app.rb", "require_relative \"dep\"\nputs VALUE\n");

    let (_r, be, bo) = run_home(&home, &["--build"], &app);
    assert!(bo, "{be}");
    let (first, _e, o) = run_home(&home, &[], &app);
    assert!(o);
    assert_eq!(first, "100\n", "runs the bundle");

    // Edit the dependency WITHOUT rebuilding: the stale bundle must be rejected
    // and the fresh source picked up via a normal compile + runtime require.
    std::fs::write(dir.join("dep.rb"), "VALUE = 999\n").unwrap();
    let (second, _e2, o2) = run_home(&home, &[], &app);
    assert!(o2);
    assert_eq!(second, "999\n", "stale bundle rejected, fresh source used");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dynamic_require_left_as_runtime_call() {
    // A `require` with a computed (non-literal) argument can't be resolved at
    // build time — it stays a runtime require and still works when the source is
    // present. The report counts it as dynamic.
    let dir = fresh_dir("dynamic");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();

    write(&dir, "plugin.rb", "def plugin_name; \"P\"; end\n");
    let app = write(
        &dir,
        "app.rb",
        "name = \"plug\" + \"in\"\n\
         require_relative name\n\
         puts plugin_name\n",
    );

    let (report, be, bo) = run_home(&home, &["--build"], &app);
    assert!(bo, "{be}");
    assert!(
        report.contains("1 file(s) bundled"),
        "only the entry bundles (dynamic require not inlined): {report}"
    );
    assert!(
        report.contains("1 runtime require"),
        "the computed require is reported dynamic: {report}"
    );

    // The bundle still runs (the runtime require resolves plugin.rb, present).
    let (out, e, o) = run_home(&home, &[], &app);
    assert!(o, "{e}");
    assert_eq!(out, "P\n");

    std::fs::remove_dir_all(&dir).ok();
}
