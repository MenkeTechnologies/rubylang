//! Appending to an array must not copy it. Pinned against MRI's own output.

use std::process::Command;

fn ruby(src: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg("-e")
        .arg(src)
        .output()
        .expect("run ruby");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `a << x` grows the array in place. It used to read the backing `Vec` out
/// through `as_array` (a full clone), append to the clone and store it back, so
/// building an array one push at a time was quadratic — 50 K pushes took 9.4 s.
///
/// The property is SCALING, and a wall-clock ceiling does not measure it: a
/// fixed "under 10 s" pinned the runner instead of the code, and a debug build
/// on a CI runner blew through it while the same code passed locally. Both
/// samples are timed INSIDE one interpreter with `Process.clock_gettime`, so
/// process startup — 1.3 s of it in a debug build, more than the small sample
/// costs — is not in either number. 4x the pushes is about 4x the time when
/// the append is in place and about 16x when it copies; the assertion sits at
/// 8x, between the two and far from both.
#[test]
fn push_is_linear_not_quadratic() {
    let out = ruby(
        "def bench(n)\n\
         \x20 t = Process.clock_gettime(Process::CLOCK_MONOTONIC)\n\
         \x20 a = []\n\
         \x20 n.times { |i| a << i }\n\
         \x20 [Process.clock_gettime(Process::CLOCK_MONOTONIC) - t, a.size]\n\
         end\n\
         small, small_n = bench(50_000)\n\
         large, large_n = bench(200_000)\n\
         puts small_n, large_n, (large / [small, 1e-6].max).round(2)\n",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "expected two sizes and a ratio, got {out:?}"
    );
    assert_eq!(lines[0], "50000");
    assert_eq!(lines[1], "200000");
    let ratio: f64 = lines[2]
        .parse()
        .unwrap_or_else(|e| panic!("ratio {:?} is not a number: {e}", lines[2]));
    assert!(
        ratio < 8.0,
        "4x the pushes cost {ratio}x the time — push is copying the array again"
    );
}

/// The in-place path must keep every observable property of the old one: the
/// receiver is returned (so `<<` chains), aliases see the append, a frozen
/// array still raises, and `push` with several arguments appends all of them.
#[test]
fn push_semantics_match_mri() {
    assert_eq!(ruby("a=[1,2]; a << 3; p a, a.size"), "[1, 2, 3]\n3\n");
    assert_eq!(ruby("a=[1,2]; a.push(3,4); p a"), "[1, 2, 3, 4]\n");
    assert_eq!(ruby("a=[]; p a.push, a"), "[]\n[]\n");
    assert_eq!(ruby("a=[1,2]; b=a; a << 3; p b"), "[1, 2, 3]\n");
    assert_eq!(ruby("a=[1,2]; x=(a << 3); p x.equal?(a)"), "true\n");
    assert_eq!(
        ruby("a=[1,2].freeze; begin; a << 3; rescue => e; p e.class; end"),
        "FrozenError\n"
    );
}

/// `size`/`length` answer without copying, and `count` — which shares the arm
/// but takes an optional block — still counts through it.
#[test]
fn length_does_not_copy_and_count_still_takes_a_block() {
    assert_eq!(ruby("a=[1,2]; p a.length, a.size, a.count"), "2\n2\n2\n");
    assert_eq!(ruby("a=[1,2,3]; p a.count { |x| x > 1 }"), "2\n");
}
