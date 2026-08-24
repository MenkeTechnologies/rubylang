//! The rkyv/bincode bytecode cache round-trips a compiled program: store then
//! load must reproduce a program that runs identically. Uses an isolated HOME so
//! a developer's real `~/.rubyrs` shard is untouched.

use rubylang::{cache, compiler, host};

#[test]
fn store_then_load_reproduces_the_program() {
    let tmp = tempfile::tempdir().unwrap();
    // Point the cache at an isolated home for the duration of this test.
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let src = "def double(x); x * 2; end; double(21)";
    let prog = rubylang::compile(src).expect("compile");
    cache::store(src, &prog).expect("store");

    let loaded = cache::load(src).expect("cached program present");
    // A different source must miss.
    assert!(cache::load("puts 1").is_none());

    // The loaded program runs to the same value as a fresh compile.
    host::reset_host();
    let compiler::Program {
        main,
        methods,
        classes,
        begins,
        procs,
    } = loaded;
    host::with_host(|h| h.load_program(methods, classes, begins, procs));
    let v = host::run_main(main).expect("run cached");
    let got = host::with_host(|h| h.inspect(&v));
    assert_eq!(got, "42");

    match prev {
        Some(p) => std::env::set_var("HOME", p),
        None => std::env::remove_var("HOME"),
    }
}

/// The shard is keyed to the BINARY that wrote it, so a rebuilt rubylang never
/// replays the previous build's bytecode.
///
/// This is the property the whole cache correctness rests on, and it was absent:
/// the key was `(SCHEMA, [path,] source)`, where `SCHEMA` is hand-bumped and
/// `CARGO_PKG_VERSION` only moves at a release, so nothing in it changed when
/// the compiler did. Proven end-to-end at the time by lowering `BinOp::Mul` to
/// `Op::Add`, rebuilding, and watching a cached `puts 6 * 7` still answer 42
/// while a fresh script answered 13.
///
/// Both halves matter. A stamp that misses a rebuild replays stale bytecode; a
/// stamp that is not stable for one unchanged binary throws the cache away on
/// every run, which is a silent performance regression rather than a wrong
/// answer, and just as invisible.
#[test]
fn the_build_stamp_separates_two_builds_and_is_stable_for_one() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");

    std::fs::write(&a, b"same-bytes").unwrap();
    assert_eq!(
        cache::exe_stamp(&a),
        cache::exe_stamp(&a),
        "one unchanged binary must keep one stamp, or every run misses the cache"
    );

    // A rebuild that changes the binary's SIZE.
    std::fs::write(&b, b"different-length-bytes").unwrap();
    assert_ne!(
        cache::exe_stamp(&a),
        cache::exe_stamp(&b),
        "two binaries of different size must not share a stamp"
    );

    // A rebuild that happens to produce the same size: only mtime separates
    // them, which is the case a size-only stamp would miss.
    std::fs::write(&b, b"same-length!").unwrap();
    std::fs::write(&a, b"same-length!").unwrap();
    let (sa, sb) = (cache::exe_stamp(&a), cache::exe_stamp(&b));
    if std::fs::metadata(&a).unwrap().modified().unwrap()
        != std::fs::metadata(&b).unwrap().modified().unwrap()
    {
        assert_ne!(
            sa, sb,
            "two same-size binaries written at different times must not share a stamp"
        );
    }

    // A path that names nothing still yields a stamp rather than panicking: an
    // embedder with no `current_exe` falls back to schema + version.
    let _ = cache::exe_stamp(&tmp.path().join("no-such-file"));
}
