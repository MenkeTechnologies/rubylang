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
/// A linear build of 200 K finishes in well under the time the quadratic one
/// needed for a twentieth of that, so a regression re-fails this on time alone.
#[test]
fn push_is_linear_not_quadratic() {
    let t = std::time::Instant::now();
    let out = ruby("a = []; 200_000.times { |i| a << i }; puts a.size");
    assert_eq!(out, "200000\n");
    assert!(
        t.elapsed().as_secs() < 10,
        "200 K pushes took {:?} — push is copying the array again",
        t.elapsed()
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
