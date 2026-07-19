//! End-to-end test for the standalone native executable (`ruby --build --native`).
//!
//! `--build --native` bundles the app, emits a Cranelift AOT object for the main
//! chunk, bakes the full program (methods/classes/blocks/consts) into a generated
//! frontend, and links a self-contained native binary next to the source. The
//! load-bearing assertion: after deleting every `.rb` source, the produced binary
//! runs the whole app — method dispatch, block yields, and namespaced constants
//! all resolve — with output identical to the interpreter (and MRI, when present)
//! and the same exit code.
//!
//! The test is gated on a working `rustc` (the linker driver `--build --native`
//! shells out to) and on the rubylang rlib being present in the build tree — both
//! true under `cargo test`, so this runs in CI on macOS/Linux; on a machine
//! without `rustc` it skips cleanly instead of failing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rubylang_aotnative_{}_{}_{}_{}",
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
    std::fs::write(&p, body).unwrap();
    p
}

/// Whether the toolchain needed to link a standalone binary is available. When
/// absent the test skips rather than failing (matches the mission's "gate on the
/// availability of a linker").
fn toolchain_ready() -> bool {
    let rustc = Command::new("rustc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let rlib = Path::new(env!("CARGO_BIN_EXE_ruby"))
        .parent()
        .map(|d| d.join("librubylang.rlib").exists())
        .unwrap_or(false);
    rustc && rlib
}

/// Run `ruby [args…] FILE`, returning (stdout, stderr, exit code, success).
fn run_ruby(args: &[&str], path: &Path) -> (String, String, i32, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .args(args)
        .arg(path)
        .output()
        .expect("spawn ruby");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
        out.status.success(),
    )
}

/// MRI stdout + exit code, if `/opt/homebrew/bin/ruby` is present (third witness).
fn mri(path: &Path) -> Option<(String, i32)> {
    let out = Command::new("/opt/homebrew/bin/ruby").arg(path).output().ok()?;
    Some((
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    ))
}

#[test]
fn native_binary_runs_whole_app_without_ruby_or_sources() {
    if !toolchain_ready() {
        eprintln!("skipping: no rustc / rubylang rlib in the build tree");
        return;
    }

    let dir = fresh_dir("standalone");

    // A required file: a namespaced class whose method uses a block (yield) and
    // reads a namespaced constant — the exact "not just a puts" shape the AOT
    // path must prove resolves in the linked binary.
    write(
        &dir,
        "geometry.rb",
        "module Geometry\n\
         \x20 PI = 3.14159\n\
         \x20 class Circle\n\
         \x20   def initialize(r); @r = r; end\n\
         \x20   def each_area(n)\n\
         \x20     n.times { |i| yield(i, @r * @r * Geometry::PI) }\n\
         \x20   end\n\
         \x20 end\n\
         end\n",
    );
    // Entry: a top-level `def` (method dispatch), the class method + block, the
    // namespaced constant, string interpolation, and a non-zero `exit`.
    let app = write(
        &dir,
        "app.rb",
        "require_relative 'geometry'\n\
         def label(i); \"slice #{i}\"; end\n\
         c = Geometry::Circle.new(2)\n\
         count = 0\n\
         c.each_area(3) do |i, area|\n\
         \x20 puts \"#{label(i)}: area=#{area}\"\n\
         \x20 count += 1\n\
         end\n\
         puts \"PI=#{Geometry::PI} slices=#{count}\"\n\
         exit 5\n",
    );

    // 1. Reference: the interpreter.
    let (direct, derr, direct_code, _) = run_ruby(&[], &app);
    assert_eq!(direct_code, 5, "interpreter exit code: {derr}");
    assert!(direct.contains("slice 0: area=12.56636"), "interp out: {direct}");

    // 2. Build the standalone native binary.
    let (report, berr, _bc, bok) = run_ruby(&["--build", "--native"], &app);
    assert!(bok, "native build failed: {berr}\n{report}");
    assert!(
        report.contains("standalone executable"),
        "report should announce a standalone executable: {report}"
    );

    // 3. Delete every source, then run the binary — no interpreter, no `.rb`.
    std::fs::remove_file(dir.join("app.rb")).unwrap();
    std::fs::remove_file(dir.join("geometry.rb")).unwrap();
    let exe = dir.join("app");
    assert!(exe.exists(), "the executable was written next to the source");

    let out = Command::new(&exe).output().expect("spawn standalone binary");
    let bin_out = String::from_utf8_lossy(&out.stdout).to_string();
    let bin_code = out.status.code().unwrap_or(-1);

    // The standalone binary reproduces the interpreter output and exit code with
    // no sources present — method call, block, and namespaced constant resolved.
    assert_eq!(bin_out, direct, "standalone stdout == interpreter stdout");
    assert_eq!(bin_code, 5, "standalone exit code propagated");

    // 4. Third witness (best-effort): MRI, before the sources were deleted, would
    // agree — but they're gone now, so re-check against the captured interpreter
    // run, which we already proved matched. (MRI parity for the same program shape
    // is covered while sources exist below.)

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn native_binary_matches_mri_when_available() {
    if !toolchain_ready() {
        eprintln!("skipping: no rustc / rubylang rlib in the build tree");
        return;
    }

    let dir = fresh_dir("mriparity");
    write(&dir, "lib.rb", "module M\n  K = 21\n  def self.double(x); x + x; end\nend\n");
    let app = write(
        &dir,
        "prog.rb",
        "require_relative 'lib'\n\
         nums = [1, 2, 3]\n\
         total = nums.reduce(0) { |a, n| a + n }\n\
         puts \"total=#{total} k=#{M::K} d=#{M.double(M::K)}\"\n",
    );

    let mri_out = mri(&app);

    let (report, berr, _bc, bok) = run_ruby(&["--build", "--native"], &app);
    assert!(bok, "native build failed: {berr}\n{report}");

    // Sources deleted; the binary must still run and agree with MRI.
    std::fs::remove_file(dir.join("prog.rb")).unwrap();
    std::fs::remove_file(dir.join("lib.rb")).unwrap();
    let out = Command::new(dir.join("prog")).output().expect("spawn binary");
    let bin_out = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "binary exit ok");
    assert_eq!(bin_out, "total=6 k=21 d=42\n", "standalone output");
    if let Some((ref_out, ref_code)) = mri_out {
        assert_eq!(bin_out, ref_out, "standalone vs MRI stdout");
        assert_eq!(out.status.code().unwrap_or(-1), ref_code, "vs MRI exit code");
    }

    std::fs::remove_dir_all(&dir).ok();
}
